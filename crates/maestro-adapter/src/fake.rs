use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use maestro_domain::{
    AgentKind, CapabilityDescriptor, CapabilityMaturity, CapabilitySupport, EventVisibility,
    IntegrationMode, NormalizedEvent, RequestId, RunId, SecurityClass, SessionId,
};
use serde_json::{Value, json};

use crate::{
    ADAPTER_CONTRACT_VERSION, AdapterCapability, AdapterConfiguration, AdapterError,
    AdapterErrorKind, AdapterEvent, AdapterEventPayload, AdapterHealth, AdapterIdentity,
    AdapterLifecycleSignal, AdapterModel, AdapterOperation, AdapterProbe, AdapterRetrySafety,
    AdapterSession, AdapterSessionSnapshot, AdapterWriterLease, AgentAdapter, AuthenticationState,
    CapabilityCatalog, CapabilityScope, ConfigurationChange, ConsoleAnnotation, FeatureInvocation,
    FeatureResult, InstallationState, PermissionDecision, PermissionResolution, ProbeRequest,
    ProcessLaunchSpec, ProcessTransport, ResumeSessionRequest, RunStopReason, StartSessionRequest,
    SteeringMode, TuiLaunchPlan, TuiLaunchRequest, TurnInput, UserInputResponse, VendorBinding,
    VendorRequestContext, VersionCompatibility,
};

const FAKE_VERSION: &str = "1.0.0";
const FAKE_EXECUTABLE: &str = "maestro-fake-agent";
const PERMISSION_REQUEST_ID: &str = "permission-1";
const USER_INPUT_REQUEST_ID: &str = "input-1";
const FAKE_CLOCK_MILLISECONDS: i64 = 0;
const FAKE_REQUEST_EXPIRY_MILLISECONDS: i64 = 1_000;

#[derive(Clone, Default)]
pub struct FakeAdapter {
    operation_log: Arc<Mutex<Vec<String>>>,
    binding_claims: Arc<Mutex<HashMap<String, RunId>>>,
}

impl fmt::Debug for FakeAdapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeAdapter")
            .field(
                "operation_count",
                &self.operation_log.lock().map_or(0, |log| log.len()),
            )
            .field(
                "binding_claim_count",
                &self.binding_claims.lock().map_or(0, |claims| claims.len()),
            )
            .finish()
    }
}

impl FakeAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns operation names only; prompts, paths, answers, and feature
    /// arguments are never copied into the deterministic audit log.
    ///
    /// # Panics
    ///
    /// Panics only if another test poisoned the internal fixture mutex.
    pub fn operation_log(&self) -> Vec<String> {
        self.operation_log
            .lock()
            .expect("fake adapter operation log mutex poisoned")
            .clone()
    }

    fn record(&self, operation: &str) {
        self.operation_log
            .lock()
            .expect("fake adapter operation log mutex poisoned")
            .push(operation.to_owned());
    }

    fn catalog() -> CapabilityCatalog {
        let capabilities = vec![
            typed_capability(
                "session.start",
                SecurityClass::Mutating,
                CapabilityScope::Project,
                "start_session",
            ),
            typed_capability(
                "session.resume",
                SecurityClass::Mutating,
                CapabilityScope::Session,
                "resume_session",
            ),
            typed_capability(
                "turn.send",
                SecurityClass::Mutating,
                CapabilityScope::Session,
                "send_turn",
            ),
            typed_capability(
                "turn.steer_or_follow_up",
                SecurityClass::Mutating,
                CapabilityScope::Session,
                "steer_or_follow_up",
            ),
            typed_capability(
                "turn.interrupt",
                SecurityClass::Mutating,
                CapabilityScope::Session,
                "interrupt",
            ),
            typed_capability(
                "permission.resolve",
                SecurityClass::Sensitive,
                CapabilityScope::Session,
                "resolve_permission",
            ),
            typed_capability(
                "user_input.respond",
                SecurityClass::Sensitive,
                CapabilityScope::Session,
                "respond_user_input",
            ),
            generic_capability(
                "feature.invoke",
                SecurityClass::Mutating,
                CapabilityScope::Session,
                "invoke_feature",
            ),
            typed_capability(
                "model.list",
                SecurityClass::ReadOnly,
                CapabilityScope::Application,
                "list_models",
            ),
            typed_capability(
                "configuration.read",
                SecurityClass::ReadOnly,
                CapabilityScope::Application,
                "read_configuration",
            ),
            typed_capability(
                "configuration.update",
                SecurityClass::Sensitive,
                CapabilityScope::Application,
                "update_configuration",
            ),
            generic_capability(
                "management.invoke",
                SecurityClass::Mutating,
                CapabilityScope::Application,
                "invoke_global_feature",
            ),
            typed_capability(
                "health.read",
                SecurityClass::ReadOnly,
                CapabilityScope::Application,
                "health_check",
            ),
            capability(
                "tui.launch",
                CapabilitySupport::PtyOnly,
                SecurityClass::Mutating,
                CapabilityScope::Session,
                "launch_tui",
                None,
            ),
        ];
        CapabilityCatalog {
            adapter_id: "maestro.fake".to_owned(),
            cli_version: FAKE_VERSION.to_owned(),
            capabilities,
        }
    }

    fn claim_binding(&self, binding: &VendorBinding, run_id: RunId) -> Result<(), AdapterError> {
        let mut claims = self
            .binding_claims
            .lock()
            .expect("fake adapter binding-claim mutex poisoned");
        if claims.contains_key(&binding.vendor_session_id) {
            return Err(AdapterError::new(
                AdapterErrorKind::BindingInUse,
                "the vendor binding already has an active writer",
            )
            .with_retry_safety(AdapterRetrySafety::Safe));
        }
        claims.insert(binding.vendor_session_id.clone(), run_id);
        Ok(())
    }

    fn session(
        &self,
        request: &StartSessionRequest,
        binding: VendorBinding,
        resumed: bool,
    ) -> Result<Box<dyn AdapterSession>, AdapterError> {
        if request.integration_mode != IntegrationMode::Structured {
            return Err(AdapterError::new(
                AdapterErrorKind::InvalidRequest,
                "the fake structured adapter requires structured integration mode",
            )
            .with_fallback(IntegrationMode::PtyTui));
        }
        if binding.agent_kind != AgentKind::Fake || binding.vendor_session_id.trim().is_empty() {
            return Err(AdapterError::new(
                AdapterErrorKind::InvalidRequest,
                "the fake adapter requires a non-empty fake vendor binding",
            ));
        }
        self.claim_binding(&binding, request.run_id)?;

        let permission_context =
            request_context(request.session_id, request.run_id, PERMISSION_REQUEST_ID);
        let user_input_context =
            request_context(request.session_id, request.run_id, USER_INPUT_REQUEST_ID);
        let mut state = FakeSessionState {
            session_id: request.session_id,
            run_id: request.run_id,
            binding_key: binding.vendor_session_id.clone(),
            binding,
            events: VecDeque::new(),
            invokable_session_features: Self::catalog()
                .capabilities
                .into_iter()
                .filter(|capability| {
                    capability.operation == "invoke_feature"
                        && capability.scopes.contains(&CapabilityScope::Session)
                })
                .map(|capability| capability.descriptor.id)
                .collect(),
            permission_requests: HashMap::from([(
                PERMISSION_REQUEST_ID.to_owned(),
                PendingDelivery::new(permission_context),
            )]),
            user_input_requests: HashMap::from([(
                USER_INPUT_REQUEST_ID.to_owned(),
                PendingDelivery::new(user_input_context),
            )]),
            expected_user_input_questions: HashSet::from(["choice".to_owned()]),
            current_time_milliseconds: FAKE_CLOCK_MILLISECONDS,
            stopped: false,
        };
        state.push(
            None,
            AdapterEventPayload::Lifecycle {
                signal: AdapterLifecycleSignal::Ready,
                detail: if resumed {
                    "fake session resumed"
                } else {
                    "fake session started"
                }
                .to_owned(),
            },
        );
        Ok(Box::new(FakeAdapterSession {
            state: Mutex::new(state),
            operation_log: Arc::clone(&self.operation_log),
            binding_claims: Arc::clone(&self.binding_claims),
        }))
    }
}

fn request_context(
    session_id: SessionId,
    run_id: RunId,
    vendor_request_id: &str,
) -> VendorRequestContext {
    VendorRequestContext {
        session_id,
        run_id,
        turn_id: None,
        item_id: None,
        vendor_request_id: vendor_request_id.to_owned(),
        expires_at_milliseconds: Some(FAKE_REQUEST_EXPIRY_MILLISECONDS),
    }
}

fn capability(
    id: &str,
    support: CapabilitySupport,
    security_class: SecurityClass,
    scope: CapabilityScope,
    operation: &str,
    input_schema: Option<Value>,
) -> AdapterCapability {
    AdapterCapability {
        descriptor: CapabilityDescriptor {
            id: id.to_owned(),
            label: id.replace(['.', '_'], " "),
            description: "Deterministic adapter-contract fixture".to_owned(),
            support,
            maturity: CapabilityMaturity::Stable,
            security_class,
            tested_version_range: Some(format!("={FAKE_VERSION}")),
            requires_authentication: false,
            fallback: (support == CapabilitySupport::PtyOnly).then(|| "Exact TUI mode".to_owned()),
            unavailable_reason: None,
        },
        scopes: vec![scope],
        operation: operation.to_owned(),
        input_schema,
        required_executables: vec![FAKE_EXECUTABLE.to_owned()],
    }
}

fn typed_capability(
    id: &str,
    security_class: SecurityClass,
    scope: CapabilityScope,
    operation: &str,
) -> AdapterCapability {
    capability(
        id,
        CapabilitySupport::Structured,
        security_class,
        scope,
        operation,
        None,
    )
}

fn generic_capability(
    id: &str,
    security_class: SecurityClass,
    scope: CapabilityScope,
    operation: &str,
) -> AdapterCapability {
    capability(
        id,
        CapabilitySupport::Structured,
        security_class,
        scope,
        operation,
        Some(json!({ "type": "object", "additionalProperties": true })),
    )
}

#[async_trait]
impl AgentAdapter for FakeAdapter {
    fn identity(&self) -> AdapterIdentity {
        AdapterIdentity {
            adapter_id: "maestro.fake".to_owned(),
            display_name: "Maestro deterministic fake agent".to_owned(),
            agent_kind: AgentKind::Fake,
            executable_names: vec![FAKE_EXECUTABLE.to_owned()],
            contract_version: ADAPTER_CONTRACT_VERSION,
        }
    }

    async fn probe(&self, _request: ProbeRequest) -> Result<AdapterProbe, AdapterError> {
        self.record("probe");
        Ok(AdapterProbe {
            installation: InstallationState::Installed,
            cli_version: Some(FAKE_VERSION.to_owned()),
            authentication: AuthenticationState::NotRequired,
            compatibility: VersionCompatibility::Tested,
            preferred_mode: IntegrationMode::Structured,
            warnings: Vec::new(),
        })
    }

    fn discover_capabilities(
        &self,
        probe: &AdapterProbe,
    ) -> Result<CapabilityCatalog, AdapterError> {
        self.record("discover_capabilities");
        if probe.compatibility != VersionCompatibility::Tested
            || probe.cli_version.as_deref() != Some(FAKE_VERSION)
        {
            return Err(AdapterError::new(
                AdapterErrorKind::UnsupportedVersion,
                "the deterministic fake adapter only supports version 1.0.0",
            )
            .with_fallback(IntegrationMode::PtyTui));
        }
        let catalog = Self::catalog();
        catalog
            .validate()
            .map_err(|error| AdapterError::new(AdapterErrorKind::Internal, error.to_string()))?;
        Ok(catalog)
    }

    async fn start_session(
        &self,
        request: StartSessionRequest,
    ) -> Result<Box<dyn AdapterSession>, AdapterError> {
        self.record("start_session");
        let binding = VendorBinding {
            agent_kind: AgentKind::Fake,
            vendor_session_id: format!("fake-{}", request.session_id),
        };
        self.session(&request, binding, false)
    }

    async fn resume_session(
        &self,
        request: ResumeSessionRequest,
    ) -> Result<Box<dyn AdapterSession>, AdapterError> {
        self.record("resume_session");
        self.session(&request.start, request.binding, true)
    }

    async fn launch_tui(&self, request: TuiLaunchRequest) -> Result<TuiLaunchPlan, AdapterError> {
        self.record("launch_tui");
        if request.start.integration_mode != IntegrationMode::PtyTui {
            return Err(AdapterError::new(
                AdapterErrorKind::InvalidRequest,
                "the exact fake TUI requires PTY/TUI integration mode",
            ));
        }
        let mut arguments = vec!["--scenario".into(), "tui/vt-baseline".into()];
        let mut writer_lease = None;
        if let Some(binding) = request.binding {
            if binding.agent_kind != AgentKind::Fake || binding.vendor_session_id.trim().is_empty()
            {
                return Err(AdapterError::new(
                    AdapterErrorKind::InvalidRequest,
                    "the exact fake TUI cannot use this vendor binding",
                ));
            }
            self.claim_binding(&binding, request.start.run_id)?;
            writer_lease = Some(Box::new(FakeBindingLease {
                binding_claims: Arc::clone(&self.binding_claims),
                binding_key: binding.vendor_session_id.clone(),
                run_id: request.start.run_id,
            }) as Box<dyn AdapterWriterLease>);
            arguments.push("--binding".into());
            arguments.push(binding.vendor_session_id.into());
        }
        Ok(TuiLaunchPlan::new(
            ProcessLaunchSpec {
                executable: request.start.executable,
                arguments,
                working_directory: request.start.working_directory,
                transport: ProcessTransport::Pty,
                requested_environment_variables: Vec::new(),
            },
            writer_lease,
        ))
    }

    async fn list_models(&self, _request: ProbeRequest) -> Result<Vec<AdapterModel>, AdapterError> {
        self.record("list_models");
        Ok(vec![AdapterModel {
            id: "fixture-model".to_owned(),
            label: "Fixture model".to_owned(),
            description: Some("Deterministic model fixture".to_owned()),
            is_default: true,
            supported_reasoning_efforts: vec!["medium".to_owned()],
            default_reasoning_effort: Some("medium".to_owned()),
        }])
    }

    async fn read_configuration(
        &self,
        _request: ProbeRequest,
    ) -> Result<AdapterConfiguration, AdapterError> {
        self.record("read_configuration");
        Ok(AdapterConfiguration::redacted(
            &json!({ "fixture": true }),
            false,
        ))
    }

    async fn update_configuration(
        &self,
        _request: ProbeRequest,
        _change: ConfigurationChange,
    ) -> Result<AdapterConfiguration, AdapterError> {
        self.record("update_configuration");
        Ok(AdapterConfiguration::redacted(
            &json!({ "fixture": true, "updated": true }),
            false,
        ))
    }

    async fn invoke_global_feature(
        &self,
        _request: ProbeRequest,
        invocation: FeatureInvocation,
    ) -> Result<FeatureResult, AdapterError> {
        self.record("invoke_global_feature");
        let supported = Self::catalog()
            .adapter_capability(&invocation.feature_id)
            .is_some_and(|capability| {
                capability.operation == "invoke_global_feature"
                    && capability.scopes.contains(&CapabilityScope::Application)
                    && capability.descriptor.is_available()
            });
        if !supported {
            return Err(AdapterError::new(
                AdapterErrorKind::UnsupportedCapability,
                "the requested feature is not an invokable global fake capability",
            ));
        }
        Ok(FeatureResult::redacted(
            invocation.operation_id,
            invocation.feature_id,
            &json!({ "status": "ok" }),
        ))
    }

    async fn health_check(&self, _request: ProbeRequest) -> Result<AdapterHealth, AdapterError> {
        self.record("health_check");
        Ok(AdapterHealth {
            healthy: true,
            summary: "deterministic fake adapter is healthy".to_owned(),
            fallback_mode: Some(IntegrationMode::PtyTui),
        })
    }
}

struct FakeAdapterSession {
    state: Mutex<FakeSessionState>,
    operation_log: Arc<Mutex<Vec<String>>>,
    binding_claims: Arc<Mutex<HashMap<String, RunId>>>,
}

struct FakeBindingLease {
    binding_claims: Arc<Mutex<HashMap<String, RunId>>>,
    binding_key: String,
    run_id: RunId,
}

impl fmt::Debug for FakeBindingLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeBindingLease")
            .field("binding", &"[VENDOR BINDING]")
            .field("run_id", &self.run_id)
            .finish_non_exhaustive()
    }
}

impl AdapterWriterLease for FakeBindingLease {}

impl Drop for FakeBindingLease {
    fn drop(&mut self) {
        release_binding_claim(&self.binding_claims, &self.binding_key, self.run_id);
    }
}

impl fmt::Debug for FakeAdapterSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock().map_err(|_| fmt::Error)?;
        formatter
            .debug_struct("FakeAdapterSession")
            .field("session_id", &state.session_id)
            .field("run_id", &state.run_id)
            .field("binding", &state.binding)
            .field("pending_event_count", &state.events.len())
            .field("stopped", &state.stopped)
            .finish_non_exhaustive()
    }
}

struct FakeSessionState {
    session_id: SessionId,
    run_id: RunId,
    binding: VendorBinding,
    binding_key: String,
    events: VecDeque<AdapterEvent>,
    invokable_session_features: HashSet<String>,
    permission_requests: HashMap<String, PendingDelivery>,
    user_input_requests: HashMap<String, PendingDelivery>,
    expected_user_input_questions: HashSet<String>,
    current_time_milliseconds: i64,
    stopped: bool,
}

impl FakeSessionState {
    fn push(&mut self, operation_id: Option<RequestId>, payload: AdapterEventPayload) {
        self.events.push_back(AdapterEvent {
            operation_id,
            payload,
        });
    }

    fn ensure_open(&self) -> Result<(), AdapterError> {
        if self.stopped {
            Err(AdapterError::new(
                AdapterErrorKind::Closed,
                "the adapter process run is stopped",
            ))
        } else {
            Ok(())
        }
    }

    fn operation(&mut self, action: &str, summary: &str) -> AdapterOperation {
        let operation_id = RequestId::new();
        self.operation_with_id(operation_id, action, summary)
    }

    fn operation_with_id(
        &mut self,
        operation_id: RequestId,
        action: &str,
        summary: &str,
    ) -> AdapterOperation {
        self.push(
            Some(operation_id),
            AdapterEventPayload::Console(ConsoleAnnotation {
                operation_id,
                action: action.to_owned(),
                summary: summary.to_owned(),
            }),
        );
        AdapterOperation { operation_id }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryState {
    Active,
    InFlight,
    Resolved,
}

#[derive(Debug)]
struct PendingDelivery {
    context: VendorRequestContext,
    state: DeliveryState,
}

impl PendingDelivery {
    fn new(context: VendorRequestContext) -> Self {
        Self {
            context,
            state: DeliveryState::Active,
        }
    }

    fn claim(
        &mut self,
        context: &VendorRequestContext,
        now_milliseconds: i64,
    ) -> Result<(), AdapterError> {
        if &self.context != context {
            return Err(AdapterError::new(
                AdapterErrorKind::RequestNotActive,
                "the single-use vendor request is not active for this session and run",
            )
            .with_retry_safety(AdapterRetrySafety::Safe));
        }
        if self
            .context
            .expires_at_milliseconds
            .is_some_and(|expiry| expiry <= now_milliseconds)
        {
            self.state = DeliveryState::Resolved;
            return Err(AdapterError::new(
                AdapterErrorKind::RequestNotActive,
                "the single-use vendor request has expired",
            ));
        }
        match self.state {
            DeliveryState::Active => {
                self.state = DeliveryState::InFlight;
                Ok(())
            }
            DeliveryState::InFlight => Err(AdapterError::new(
                AdapterErrorKind::RequestNotActive,
                "delivery of the single-use vendor request is still uncertain",
            )
            .with_retry_safety(AdapterRetrySafety::UnsafeDeliveryUncertain)),
            DeliveryState::Resolved => Err(AdapterError::new(
                AdapterErrorKind::RequestNotActive,
                "the single-use vendor request is already resolved",
            )),
        }
    }

    fn confirm_delivery(&mut self) {
        debug_assert_eq!(self.state, DeliveryState::InFlight);
        self.state = DeliveryState::Resolved;
    }

    #[cfg(test)]
    fn delivery_failed(&mut self, safety: AdapterRetrySafety) {
        debug_assert_eq!(self.state, DeliveryState::InFlight);
        if safety == AdapterRetrySafety::Safe {
            self.state = DeliveryState::Active;
        }
    }
}

impl FakeAdapterSession {
    fn record(&self, operation: &str) {
        self.operation_log
            .lock()
            .expect("fake adapter operation log mutex poisoned")
            .push(operation.to_owned());
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, FakeSessionState> {
        self.state
            .lock()
            .expect("fake adapter session mutex poisoned")
    }

    fn release_binding_claim(&self, binding_key: &str, run_id: RunId) {
        release_binding_claim(&self.binding_claims, binding_key, run_id);
    }
}

fn release_binding_claim(
    binding_claims: &Mutex<HashMap<String, RunId>>,
    binding_key: &str,
    run_id: RunId,
) {
    let Ok(mut claims) = binding_claims.lock() else {
        return;
    };
    if claims.get(binding_key) == Some(&run_id) {
        claims.remove(binding_key);
    }
}

impl Drop for FakeAdapterSession {
    fn drop(&mut self) {
        let Ok(state) = self.state.lock() else {
            return;
        };
        self.release_binding_claim(&state.binding_key, state.run_id);
    }
}

#[async_trait]
impl AdapterSession for FakeAdapterSession {
    fn snapshot(&self) -> AdapterSessionSnapshot {
        let state = self.locked();
        AdapterSessionSnapshot {
            session_id: state.session_id,
            run_id: state.run_id,
            binding: state.binding.clone(),
        }
    }

    async fn send_turn(&self, _input: TurnInput) -> Result<AdapterOperation, AdapterError> {
        self.record("send_turn");
        let mut state = self.locked();
        state.ensure_open()?;
        let operation = state.operation("turn.send(...)", "GUI requested a fake turn");
        state.push(
            Some(operation.operation_id),
            AdapterEventPayload::Normalized(NormalizedEvent {
                kind: "turn.accepted".to_owned(),
                visibility: EventVisibility::User,
                payload: json!({ "status": "accepted" }),
                vendor_event_id: None,
                raw_segment_reference: None,
            }),
        );
        Ok(operation)
    }

    async fn steer_or_follow_up(
        &self,
        mode: SteeringMode,
        _input: TurnInput,
    ) -> Result<AdapterOperation, AdapterError> {
        self.record("steer_or_follow_up");
        let mut state = self.locked();
        state.ensure_open()?;
        let action = match mode {
            SteeringMode::SteerActiveTurn => "turn.steer(...)",
            SteeringMode::QueueFollowUp => "turn.follow_up(...)",
        };
        Ok(state.operation(action, "GUI supplied additional fake input"))
    }

    async fn interrupt(&self) -> Result<AdapterOperation, AdapterError> {
        self.record("interrupt");
        let mut state = self.locked();
        state.ensure_open()?;
        Ok(state.operation("turn.interrupt(...)", "GUI interrupted the fake turn"))
    }

    async fn resolve_permission(
        &self,
        resolution: PermissionResolution,
    ) -> Result<AdapterOperation, AdapterError> {
        self.record("resolve_permission");
        let mut state = self.locked();
        state.ensure_open()?;
        let now_milliseconds = state.current_time_milliseconds;
        let pending = state
            .permission_requests
            .get_mut(&resolution.context.vendor_request_id)
            .ok_or_else(|| {
                AdapterError::new(
                    AdapterErrorKind::RequestNotActive,
                    "the permission request is not active",
                )
            })?;
        pending.claim(&resolution.context, now_milliseconds)?;
        pending.confirm_delivery();
        Ok(state.operation(
            "permission.resolve(...)",
            permission_decision_summary(&resolution.decision),
        ))
    }

    async fn respond_user_input(
        &self,
        response: UserInputResponse,
    ) -> Result<AdapterOperation, AdapterError> {
        self.record("respond_user_input");
        let mut state = self.locked();
        state.ensure_open()?;
        let now_milliseconds = state.current_time_milliseconds;
        let mut answer_ids = HashSet::new();
        if response.answers.iter().any(|answer| {
            answer.question_id.trim().is_empty()
                || !state
                    .expected_user_input_questions
                    .contains(&answer.question_id)
                || !answer_ids.insert(answer.question_id.clone())
        }) {
            return Err(AdapterError::new(
                AdapterErrorKind::InvalidRequest,
                "user-input answers must use unique expected question identifiers",
            )
            .with_retry_safety(AdapterRetrySafety::Safe));
        }
        let pending = state
            .user_input_requests
            .get_mut(&response.context.vendor_request_id)
            .ok_or_else(|| {
                AdapterError::new(
                    AdapterErrorKind::RequestNotActive,
                    "the user-input request is not active",
                )
            })?;
        pending.claim(&response.context, now_milliseconds)?;
        pending.confirm_delivery();
        Ok(state.operation(
            "user_input.respond(...)",
            "GUI returned redacted keyed fake answers",
        ))
    }

    async fn invoke_feature(
        &self,
        invocation: FeatureInvocation,
    ) -> Result<FeatureResult, AdapterError> {
        self.record("invoke_feature");
        let mut state = self.locked();
        state.ensure_open()?;
        if !state
            .invokable_session_features
            .contains(&invocation.feature_id)
        {
            return Err(AdapterError::new(
                AdapterErrorKind::UnsupportedCapability,
                "the requested feature is not an invokable session fake capability",
            ));
        }
        state.operation_with_id(
            invocation.operation_id,
            "feature.invoke(...)",
            "GUI invoked a fake adapter feature",
        );
        Ok(FeatureResult::redacted(
            invocation.operation_id,
            invocation.feature_id,
            &json!({ "status": "ok" }),
        ))
    }

    async fn next_event(&self) -> Result<Option<AdapterEvent>, AdapterError> {
        self.record("next_event");
        Ok(self.locked().events.pop_front())
    }

    async fn stop_run(&self, _reason: RunStopReason) -> Result<(), AdapterError> {
        self.record("stop_run");
        let (binding_key, run_id) = {
            let mut state = self.locked();
            if !state.stopped {
                state.stopped = true;
                state.push(
                    None,
                    AdapterEventPayload::Lifecycle {
                        signal: AdapterLifecycleSignal::Stopped,
                        detail: "fake adapter process run stopped".to_owned(),
                    },
                );
            }
            (state.binding_key.clone(), state.run_id)
        };
        self.release_binding_claim(&binding_key, run_id);
        Ok(())
    }
}

fn permission_decision_summary(decision: &PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::AllowOnce => "GUI allowed one fake permission request",
        PermissionDecision::AllowForSession => "GUI allowed a fake permission for this session",
        PermissionDecision::Deny => "GUI denied one fake permission request",
        PermissionDecision::Cancel => "GUI cancelled one fake permission request",
        PermissionDecision::VendorSpecific { .. } => {
            "GUI returned a vendor-specific fake permission decision"
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, path::PathBuf};

    use maestro_domain::{ProjectId, SessionId};

    use super::*;
    use crate::{InputItem, SensitiveJson, SensitiveText, SessionOptions, UserInputAnswer};

    fn probe_request() -> ProbeRequest {
        ProbeRequest {
            executable: PathBuf::from(FAKE_EXECUTABLE),
            working_directory: PathBuf::from("/fixture"),
        }
    }

    fn start_request(
        session_id: SessionId,
        run_id: RunId,
        integration_mode: IntegrationMode,
    ) -> StartSessionRequest {
        StartSessionRequest {
            session_id,
            run_id,
            project_id: ProjectId::new(),
            executable: PathBuf::from(FAKE_EXECUTABLE),
            working_directory: PathBuf::from("/fixture"),
            integration_mode,
            options: SessionOptions::default(),
            capture_raw_protocol: false,
        }
    }

    fn structured_request(session_id: SessionId, run_id: RunId) -> StartSessionRequest {
        start_request(session_id, run_id, IntegrationMode::Structured)
    }

    fn context(session_id: SessionId, run_id: RunId, request_id: &str) -> VendorRequestContext {
        request_context(session_id, run_id, request_id)
    }

    fn turn(text: &str) -> TurnInput {
        TurnInput {
            items: vec![InputItem::Text {
                text: SensitiveText::new(text),
            }],
        }
    }

    #[tokio::test]
    async fn fake_reference_covers_the_complete_contract() {
        let adapter = FakeAdapter::new();
        let identity = adapter.identity();
        assert_eq!(identity.adapter_id, "maestro.fake");
        assert_eq!(identity.contract_version, ADAPTER_CONTRACT_VERSION);

        let probe = adapter.probe(probe_request()).await.expect("probe");
        let catalog = adapter.discover_capabilities(&probe).expect("capabilities");
        assert_eq!(catalog.capabilities.len(), 14);

        let session_id = SessionId::new();
        let run_id = RunId::new();
        let session = adapter
            .start_session(structured_request(session_id, run_id))
            .await
            .expect("session");
        assert_eq!(session.snapshot().run_id, run_id);
        session
            .send_turn(turn("sensitive prompt"))
            .await
            .expect("turn");
        session
            .steer_or_follow_up(SteeringMode::SteerActiveTurn, turn("sensitive steering"))
            .await
            .expect("steer");
        session
            .steer_or_follow_up(SteeringMode::QueueFollowUp, turn("sensitive follow-up"))
            .await
            .expect("follow-up");
        session.interrupt().await.expect("interrupt");
        session
            .resolve_permission(PermissionResolution {
                context: context(session_id, run_id, PERMISSION_REQUEST_ID),
                decision: PermissionDecision::Deny,
            })
            .await
            .expect("permission");
        session
            .respond_user_input(UserInputResponse {
                context: context(session_id, run_id, USER_INPUT_REQUEST_ID),
                answers: vec![UserInputAnswer {
                    question_id: "choice".to_owned(),
                    values: vec![SensitiveText::new("sensitive answer")],
                }],
            })
            .await
            .expect("input");
        session
            .invoke_feature(
                FeatureInvocation::new(
                    RequestId::new(),
                    "feature.invoke",
                    &json!({ "secret": "feature argument" }),
                )
                .expect("feature invocation"),
            )
            .await
            .expect("feature");

        let binding = session.snapshot().binding;
        session
            .stop_run(RunStopReason::SwitchIntegrationMode)
            .await
            .expect("stop run");
        let resumed_run = RunId::new();
        let resumed = adapter
            .resume_session(ResumeSessionRequest {
                start: structured_request(session_id, resumed_run),
                binding: binding.clone(),
            })
            .await
            .expect("resume");
        assert_eq!(resumed.snapshot().run_id, resumed_run);
        resumed
            .stop_run(RunStopReason::SwitchIntegrationMode)
            .await
            .expect("stop resumed run");

        assert!(
            adapter
                .health_check(probe_request())
                .await
                .expect("health")
                .healthy
        );

        let operations = adapter.operation_log();
        for expected in [
            "probe",
            "discover_capabilities",
            "start_session",
            "send_turn",
            "steer_or_follow_up",
            "interrupt",
            "resolve_permission",
            "respond_user_input",
            "invoke_feature",
            "stop_run",
            "resume_session",
            "health_check",
        ] {
            assert!(operations.iter().any(|operation| operation == expected));
        }
        assert!(!format!("{operations:?}").contains("sensitive"));
    }

    #[tokio::test]
    async fn exact_tui_and_structured_runs_share_one_binding_writer_claim() {
        let adapter = FakeAdapter::new();
        let session_id = SessionId::new();
        let structured = adapter
            .start_session(structured_request(session_id, RunId::new()))
            .await
            .expect("structured session");
        let binding = structured.snapshot().binding;
        let active_tui = adapter
            .launch_tui(TuiLaunchRequest {
                start: start_request(SessionId::new(), RunId::new(), IntegrationMode::PtyTui),
                binding: Some(binding.clone()),
            })
            .await
            .expect_err("active structured writer excludes TUI");
        assert_eq!(active_tui.kind(), AdapterErrorKind::BindingInUse);

        structured
            .stop_run(RunStopReason::SwitchIntegrationMode)
            .await
            .expect("release structured binding");
        let tui_run_id = RunId::new();
        let tui = adapter
            .launch_tui(TuiLaunchRequest {
                start: start_request(SessionId::new(), tui_run_id, IntegrationMode::PtyTui),
                binding: Some(binding.clone()),
            })
            .await
            .expect("TUI");

        assert_eq!(tui.process().transport, ProcessTransport::Pty);
        assert!(
            tui.process()
                .arguments
                .iter()
                .any(|value| value == OsStr::new("--binding"))
        );
        assert_eq!(
            tui.process().arguments.last().map(AsRef::as_ref),
            Some(OsStr::new(&binding.vendor_session_id))
        );

        let blocked_resume = adapter
            .resume_session(ResumeSessionRequest {
                start: structured_request(session_id, RunId::new()),
                binding: binding.clone(),
            })
            .await
            .expect_err("active TUI writer excludes structured resume");
        assert_eq!(blocked_resume.kind(), AdapterErrorKind::BindingInUse);

        drop(tui);
        adapter
            .resume_session(ResumeSessionRequest {
                start: structured_request(session_id, RunId::new()),
                binding,
            })
            .await
            .expect("dropping the TUI plan releases its writer lease");
    }

    #[tokio::test]
    async fn fake_reference_covers_global_management_operations() {
        let adapter = FakeAdapter::new();
        assert_eq!(
            adapter
                .list_models(probe_request())
                .await
                .expect("models")
                .len(),
            1
        );
        assert!(
            !adapter
                .read_configuration(probe_request())
                .await
                .expect("configuration")
                .is_read_only()
        );
        adapter
            .update_configuration(
                probe_request(),
                ConfigurationChange::new("fixture.enabled", &json!(true))
                    .expect("configuration change"),
            )
            .await
            .expect("configuration update");
        let operation_id = RequestId::new();
        let result = adapter
            .invoke_global_feature(
                probe_request(),
                FeatureInvocation::new(
                    operation_id,
                    "management.invoke",
                    &json!({ "operation": "fixture" }),
                )
                .expect("management invocation"),
            )
            .await
            .expect("global feature");
        assert_eq!(result.operation_id, operation_id);
    }

    #[tokio::test]
    async fn event_order_and_operation_correlation_are_adapter_owned_but_sequence_is_not() {
        let adapter = FakeAdapter::new();
        let session = adapter
            .start_session(structured_request(SessionId::new(), RunId::new()))
            .await
            .expect("session");
        let operation = session
            .send_turn(turn("do not log me"))
            .await
            .expect("turn");

        let ready = session.next_event().await.expect("event").expect("ready");
        let console = session.next_event().await.expect("event").expect("console");
        let accepted = session
            .next_event()
            .await
            .expect("event")
            .expect("accepted");
        assert!(matches!(
            ready.payload,
            AdapterEventPayload::Lifecycle {
                signal: AdapterLifecycleSignal::Ready,
                ..
            }
        ));
        assert_eq!(console.operation_id, Some(operation.operation_id));
        assert_eq!(accepted.operation_id, Some(operation.operation_id));
        match console.payload {
            AdapterEventPayload::Console(annotation) => {
                assert_eq!(annotation.operation_id, operation.operation_id);
            }
            other => panic!("expected console annotation, got {other:?}"),
        }
        assert!(!format!("{accepted:?}").contains("do not log me"));
    }

    #[tokio::test]
    async fn concurrent_generic_feature_results_retain_daemon_operation_correlation() {
        let adapter = FakeAdapter::new();
        let session = adapter
            .start_session(structured_request(SessionId::new(), RunId::new()))
            .await
            .expect("session");
        let first_id = RequestId::new();
        let second_id = RequestId::new();
        let (first, second) = tokio::join!(
            session.invoke_feature(
                FeatureInvocation::new(first_id, "feature.invoke", &json!({ "call": 1 }))
                    .expect("first invocation"),
            ),
            session.invoke_feature(
                FeatureInvocation::new(second_id, "feature.invoke", &json!({ "call": 2 }))
                    .expect("second invocation"),
            )
        );

        assert_eq!(first.expect("first result").operation_id, first_id);
        assert_eq!(second.expect("second result").operation_id, second_id);
        let mut correlated = HashSet::new();
        while let Some(event) = session.next_event().await.expect("event") {
            if let AdapterEventPayload::Console(annotation) = event.payload {
                correlated.insert(annotation.operation_id);
            }
        }
        assert!(correlated.contains(&first_id));
        assert!(correlated.contains(&second_id));
    }

    #[tokio::test]
    async fn feature_invocation_rejects_cross_scope_and_unknown_capabilities() {
        let adapter = FakeAdapter::new();
        let session = adapter
            .start_session(structured_request(SessionId::new(), RunId::new()))
            .await
            .expect("session");

        for feature_id in ["management.invoke", "turn.send", "missing"] {
            let error = session
                .invoke_feature(
                    FeatureInvocation::new(RequestId::new(), feature_id, &json!({}))
                        .expect("feature invocation"),
                )
                .await
                .expect_err("session scope must reject feature");
            assert_eq!(error.kind(), AdapterErrorKind::UnsupportedCapability);
        }
        for feature_id in ["feature.invoke", "turn.send", "missing"] {
            let error = adapter
                .invoke_global_feature(
                    probe_request(),
                    FeatureInvocation::new(RequestId::new(), feature_id, &json!({}))
                        .expect("feature invocation"),
                )
                .await
                .expect_err("global scope must reject feature");
            assert_eq!(error.kind(), AdapterErrorKind::UnsupportedCapability);
        }
    }

    #[tokio::test]
    async fn vendor_binding_has_only_one_active_writer_and_releases_on_stop() {
        let adapter = FakeAdapter::new();
        let session_id = SessionId::new();
        let first = adapter
            .start_session(structured_request(session_id, RunId::new()))
            .await
            .expect("first writer");
        let binding = first.snapshot().binding;
        let duplicate = adapter
            .resume_session(ResumeSessionRequest {
                start: structured_request(session_id, RunId::new()),
                binding: binding.clone(),
            })
            .await
            .expect_err("duplicate writer must fail");
        assert_eq!(duplicate.kind(), AdapterErrorKind::BindingInUse);
        assert_eq!(duplicate.retry_safety(), AdapterRetrySafety::Safe);

        first
            .stop_run(RunStopReason::UserRequested)
            .await
            .expect("release first writer");
        adapter
            .resume_session(ResumeSessionRequest {
                start: structured_request(session_id, RunId::new()),
                binding,
            })
            .await
            .expect("writer claim released");
    }

    #[tokio::test]
    async fn permission_variants_are_single_use_and_scoped_to_session_and_run() {
        let decisions = vec![
            PermissionDecision::AllowOnce,
            PermissionDecision::AllowForSession,
            PermissionDecision::Deny,
            PermissionDecision::Cancel,
            PermissionDecision::VendorSpecific {
                decision_id: "fixture-choice".to_owned(),
                payload: Some(
                    SensitiveJson::from_value(&json!({ "token": "secret-value" }))
                        .expect("sensitive JSON"),
                ),
            },
        ];

        for decision in decisions {
            let adapter = FakeAdapter::new();
            let session_id = SessionId::new();
            let run_id = RunId::new();
            let session = adapter
                .start_session(structured_request(session_id, run_id))
                .await
                .expect("session");
            let wrong = session
                .resolve_permission(PermissionResolution {
                    context: context(session_id, RunId::new(), PERMISSION_REQUEST_ID),
                    decision: PermissionDecision::Deny,
                })
                .await
                .expect_err("wrong run must fail");
            assert_eq!(wrong.kind(), AdapterErrorKind::RequestNotActive);
            assert_eq!(wrong.retry_safety(), AdapterRetrySafety::Safe);

            let resolution = PermissionResolution {
                context: context(session_id, run_id, PERMISSION_REQUEST_ID),
                decision,
            };
            session
                .resolve_permission(resolution)
                .await
                .expect("active request");
            let duplicate = session
                .resolve_permission(PermissionResolution {
                    context: context(session_id, run_id, PERMISSION_REQUEST_ID),
                    decision: PermissionDecision::Deny,
                })
                .await
                .expect_err("resolved request is single use");
            assert_eq!(duplicate.kind(), AdapterErrorKind::RequestNotActive);
        }
    }

    #[tokio::test]
    async fn keyed_user_input_is_single_use_and_closed_runs_reject_operations() {
        let adapter = FakeAdapter::new();
        let session_id = SessionId::new();
        let run_id = RunId::new();
        let session = adapter
            .start_session(structured_request(session_id, run_id))
            .await
            .expect("session");
        let unknown_question = session
            .respond_user_input(UserInputResponse {
                context: context(session_id, run_id, USER_INPUT_REQUEST_ID),
                answers: vec![UserInputAnswer {
                    question_id: "unknown".to_owned(),
                    values: vec![SensitiveText::new("alpha")],
                }],
            })
            .await
            .expect_err("unknown question must fail before delivery");
        assert_eq!(unknown_question.kind(), AdapterErrorKind::InvalidRequest);
        assert_eq!(unknown_question.retry_safety(), AdapterRetrySafety::Safe);
        session
            .respond_user_input(UserInputResponse {
                context: context(session_id, run_id, USER_INPUT_REQUEST_ID),
                answers: vec![UserInputAnswer {
                    question_id: "choice".to_owned(),
                    values: vec![SensitiveText::new("alpha")],
                }],
            })
            .await
            .expect("input response");
        let duplicate = session
            .respond_user_input(UserInputResponse {
                context: context(session_id, run_id, USER_INPUT_REQUEST_ID),
                answers: Vec::new(),
            })
            .await
            .expect_err("input response is single use");
        assert_eq!(duplicate.kind(), AdapterErrorKind::RequestNotActive);

        session
            .stop_run(RunStopReason::UserRequested)
            .await
            .expect("stop");
        assert_eq!(
            session
                .send_turn(turn("closed"))
                .await
                .expect_err("closed")
                .kind(),
            AdapterErrorKind::Closed
        );
    }

    #[tokio::test]
    async fn unsupported_versions_modes_and_bindings_expose_safe_fallbacks() {
        let adapter = FakeAdapter::new();
        let unsupported = adapter
            .discover_capabilities(&AdapterProbe {
                installation: InstallationState::Installed,
                cli_version: Some("2.0.0".to_owned()),
                authentication: AuthenticationState::NotRequired,
                compatibility: VersionCompatibility::Untested,
                preferred_mode: IntegrationMode::PtyTui,
                warnings: Vec::new(),
            })
            .expect_err("unsupported version");
        assert_eq!(unsupported.kind(), AdapterErrorKind::UnsupportedVersion);
        assert_eq!(unsupported.fallback_mode(), Some(IntegrationMode::PtyTui));

        let wrong_mode = adapter
            .start_session(start_request(
                SessionId::new(),
                RunId::new(),
                IntegrationMode::CliManaged,
            ))
            .await
            .expect_err("wrong mode");
        assert_eq!(wrong_mode.fallback_mode(), Some(IntegrationMode::PtyTui));

        let wrong_binding = adapter
            .resume_session(ResumeSessionRequest {
                start: structured_request(SessionId::new(), RunId::new()),
                binding: VendorBinding {
                    agent_kind: AgentKind::Codex,
                    vendor_session_id: "not-fake".to_owned(),
                },
            })
            .await
            .expect_err("wrong binding");
        assert_eq!(wrong_binding.kind(), AdapterErrorKind::InvalidRequest);
    }

    #[test]
    fn delivery_state_restores_only_definite_non_delivery() {
        let session_id = SessionId::new();
        let run_id = RunId::new();
        let context = context(session_id, run_id, PERMISSION_REQUEST_ID);
        let mut pending = PendingDelivery::new(context.clone());

        pending
            .claim(&context, FAKE_CLOCK_MILLISECONDS)
            .expect("claim");
        assert_eq!(pending.state, DeliveryState::InFlight);
        pending.delivery_failed(AdapterRetrySafety::Safe);
        assert_eq!(pending.state, DeliveryState::Active);

        pending
            .claim(&context, FAKE_CLOCK_MILLISECONDS)
            .expect("reclaim");
        pending.delivery_failed(AdapterRetrySafety::UnsafeDeliveryUncertain);
        assert_eq!(pending.state, DeliveryState::InFlight);
        assert_eq!(
            pending
                .claim(&context, FAKE_CLOCK_MILLISECONDS)
                .expect_err("unsafe retry blocked")
                .retry_safety(),
            AdapterRetrySafety::UnsafeDeliveryUncertain
        );
    }

    #[test]
    fn expired_delivery_is_authoritatively_resolved_without_child_delivery() {
        let mut context = context(SessionId::new(), RunId::new(), PERMISSION_REQUEST_ID);
        context.expires_at_milliseconds = Some(5);
        let mut pending = PendingDelivery::new(context.clone());

        let error = pending.claim(&context, 5).expect_err("request expired");
        assert_eq!(error.kind(), AdapterErrorKind::RequestNotActive);
        assert_eq!(error.retry_safety(), AdapterRetrySafety::NotApplicable);
        assert_eq!(pending.state, DeliveryState::Resolved);
    }
}
