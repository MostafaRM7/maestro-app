use std::path::PathBuf;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{AgentKind, ProjectId, RequestId, SessionId};

/// The decision represented by a user-created Maestro rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionEffect {
    Allow,
    Deny,
}

/// The supported scope hierarchy, from one request through all CLIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", content = "id", rename_all = "snake_case")]
pub enum PermissionRuleScope {
    Request(RequestId),
    Session(SessionId),
    Project(ProjectId),
    Cli(AgentKind),
    Global,
}

impl PermissionRuleScope {
    fn specificity(&self) -> u8 {
        match self {
            Self::Request(_) => 5,
            Self::Session(_) => 4,
            Self::Project(_) => 3,
            Self::Cli(_) => 2,
            Self::Global => 1,
        }
    }

    fn matches(&self, request: &PermissionRequestContext) -> bool {
        match self {
            Self::Request(id) => *id == request.request_id,
            Self::Session(id) => *id == request.session_id,
            Self::Project(id) => *id == request.project_id,
            Self::Cli(agent) => *agent == request.agent,
            Self::Global => true,
        }
    }
}

/// Optional constraints applied in addition to a rule's scope.
///
/// `canonical_path_prefix` is intentionally a path boundary comparison rather
/// than a string prefix. Callers must supply already-canonical request paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRuleMatcher {
    pub tool_name: Option<String>,
    pub command_regex: Option<String>,
    pub canonical_path_prefix: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRule {
    pub id: String,
    pub effect: PermissionEffect,
    pub scope: PermissionRuleScope,
    pub matcher: PermissionRuleMatcher,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub creation_source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequestContext {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub project_id: ProjectId,
    pub agent: AgentKind,
    pub tool_name: Option<String>,
    pub command: Option<String>,
    pub canonical_paths: Vec<PathBuf>,
    pub dangerous: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorDefaultDecision {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorPermissionPolicy {
    pub admin_or_vendor_denied: bool,
    pub explicitly_asks: bool,
    pub default_decision: VendorDefaultDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionEvaluationSource {
    VendorOrAdminDeny,
    MaestroRule(String),
    VendorExplicitAsk,
    VendorDefault,
    GuiPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionEvaluation {
    pub effect: Option<PermissionEffect>,
    pub source: PermissionEvaluationSource,
}

/// Evaluates the approved overlay order. In particular, a Maestro allow rule
/// can never bypass a vendor/admin deny or an explicit vendor prompt.
pub fn evaluate_permission(
    request: &PermissionRequestContext,
    vendor: VendorPermissionPolicy,
    rules: &[PermissionRule],
    now: DateTime<Utc>,
) -> PermissionEvaluation {
    if vendor.admin_or_vendor_denied {
        return evaluation(
            PermissionEffect::Deny,
            PermissionEvaluationSource::VendorOrAdminDeny,
        );
    }

    if let Some(rule) = best_matching_rule(request, rules, now, PermissionEffect::Deny) {
        return evaluation(
            PermissionEffect::Deny,
            PermissionEvaluationSource::MaestroRule(rule.id.clone()),
        );
    }

    if vendor.explicitly_asks {
        return PermissionEvaluation {
            effect: None,
            source: PermissionEvaluationSource::VendorExplicitAsk,
        };
    }

    if let Some(rule) = best_matching_rule(request, rules, now, PermissionEffect::Allow) {
        return evaluation(
            PermissionEffect::Allow,
            PermissionEvaluationSource::MaestroRule(rule.id.clone()),
        );
    }

    match vendor.default_decision {
        VendorDefaultDecision::Allow => evaluation(
            PermissionEffect::Allow,
            PermissionEvaluationSource::VendorDefault,
        ),
        VendorDefaultDecision::Deny => evaluation(
            PermissionEffect::Deny,
            PermissionEvaluationSource::VendorDefault,
        ),
        VendorDefaultDecision::Ask => PermissionEvaluation {
            effect: None,
            source: PermissionEvaluationSource::GuiPrompt,
        },
    }
}

fn evaluation(
    effect: PermissionEffect,
    source: PermissionEvaluationSource,
) -> PermissionEvaluation {
    PermissionEvaluation {
        effect: Some(effect),
        source,
    }
}

fn best_matching_rule<'a>(
    request: &PermissionRequestContext,
    rules: &'a [PermissionRule],
    now: DateTime<Utc>,
    effect: PermissionEffect,
) -> Option<&'a PermissionRule> {
    rules
        .iter()
        .filter(|rule| rule.effect == effect)
        .filter(|rule| rule.expires_at.is_none_or(|expires_at| expires_at > now))
        .filter(|rule| rule.scope.matches(request))
        .filter(|rule| matcher_matches(&rule.matcher, request, effect))
        .max_by_key(|rule| rule.scope.specificity())
}

fn matcher_matches(
    matcher: &PermissionRuleMatcher,
    request: &PermissionRequestContext,
    effect: PermissionEffect,
) -> bool {
    if let Some(expected) = &matcher.tool_name
        && request.tool_name.as_deref() != Some(expected.as_str())
    {
        return false;
    }
    if let Some(pattern) = &matcher.command_regex {
        let Some(command) = &request.command else {
            return false;
        };
        let Ok(regex) = Regex::new(pattern) else {
            return false;
        };
        if !regex.is_match(command) {
            return false;
        }
    }
    if let Some(prefix) = &matcher.canonical_path_prefix {
        if request.canonical_paths.is_empty() {
            return false;
        }
        return match effect {
            PermissionEffect::Allow => request
                .canonical_paths
                .iter()
                .all(|path| path.starts_with(prefix)),
            PermissionEffect::Deny => request
                .canonical_paths
                .iter()
                .any(|path| path.starts_with(prefix)),
        };
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRuleDraft {
    pub id: String,
    pub effect: PermissionEffect,
    pub scope: PermissionRuleScope,
    pub matcher: PermissionRuleMatcher,
    pub expires_at: Option<DateTime<Utc>>,
    pub creation_source: String,
    pub dangerous: bool,
}

/// Persistence is impossible without the explicit `Remember` variant. A
/// one-request approval produces no stored rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleCreationIntent {
    ThisRequestOnly,
    Remember {
        global_confirmed: bool,
        dangerous_confirmed: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PermissionRuleCreationError {
    #[error("global permission rules require a second confirmation")]
    GlobalConfirmationRequired,
    #[error("dangerous permission rules require a stronger confirmation")]
    DangerousConfirmationRequired,
}

/// Creates a durable rule only from an explicit remember intent.
///
/// # Errors
///
/// Returns [`PermissionRuleCreationError`] when a global or dangerous rule is
/// missing its required stronger confirmation.
pub fn create_persistent_permission_rule(
    draft: PermissionRuleDraft,
    intent: RuleCreationIntent,
    now: DateTime<Utc>,
) -> Result<Option<PermissionRule>, PermissionRuleCreationError> {
    let RuleCreationIntent::Remember {
        global_confirmed,
        dangerous_confirmed,
    } = intent
    else {
        return Ok(None);
    };
    if matches!(draft.scope, PermissionRuleScope::Global) && !global_confirmed {
        return Err(PermissionRuleCreationError::GlobalConfirmationRequired);
    }
    if draft.dangerous && !dangerous_confirmed {
        return Err(PermissionRuleCreationError::DangerousConfirmationRequired);
    }
    Ok(Some(PermissionRule {
        id: draft.id,
        effect: draft.effect,
        scope: draft.scope,
        matcher: draft.matcher,
        expires_at: draft.expires_at,
        created_at: now,
        creation_source: draft.creation_source,
    }))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::Duration;

    use super::*;

    fn request() -> PermissionRequestContext {
        PermissionRequestContext {
            request_id: RequestId::new(),
            session_id: SessionId::new(),
            project_id: ProjectId::new(),
            agent: AgentKind::Codex,
            tool_name: Some("shell".to_owned()),
            command: Some("git status --short".to_owned()),
            canonical_paths: vec![PathBuf::from("/workspace/repo/src")],
            dangerous: false,
        }
    }

    fn rule(effect: PermissionEffect, scope: PermissionRuleScope) -> PermissionRule {
        PermissionRule {
            id: format!("{effect:?}"),
            effect,
            scope,
            matcher: PermissionRuleMatcher::default(),
            expires_at: None,
            created_at: Utc::now(),
            creation_source: "test".to_owned(),
        }
    }

    #[test]
    fn vendor_or_admin_deny_always_wins_over_maestro_allow() {
        let request = request();
        let result = evaluate_permission(
            &request,
            VendorPermissionPolicy {
                admin_or_vendor_denied: true,
                explicitly_asks: false,
                default_decision: VendorDefaultDecision::Allow,
            },
            &[rule(PermissionEffect::Allow, PermissionRuleScope::Global)],
            Utc::now(),
        );
        assert_eq!(result.effect, Some(PermissionEffect::Deny));
        assert_eq!(result.source, PermissionEvaluationSource::VendorOrAdminDeny);
    }

    #[test]
    fn maestro_deny_wins_before_vendor_ask_but_vendor_ask_blocks_maestro_allow() {
        let request = request();
        let now = Utc::now();
        let vendor_ask = VendorPermissionPolicy {
            admin_or_vendor_denied: false,
            explicitly_asks: true,
            default_decision: VendorDefaultDecision::Allow,
        };
        let denied = evaluate_permission(
            &request,
            vendor_ask,
            &[rule(PermissionEffect::Deny, PermissionRuleScope::Global)],
            now,
        );
        assert_eq!(denied.effect, Some(PermissionEffect::Deny));

        let asked = evaluate_permission(
            &request,
            vendor_ask,
            &[rule(PermissionEffect::Allow, PermissionRuleScope::Global)],
            now,
        );
        assert_eq!(asked.effect, None);
        assert_eq!(asked.source, PermissionEvaluationSource::VendorExplicitAsk);
    }

    #[test]
    fn matching_is_scope_expiration_command_and_path_aware() {
        let request = request();
        let now = Utc::now();
        let mut matching = rule(
            PermissionEffect::Allow,
            PermissionRuleScope::Session(request.session_id),
        );
        matching.id = "matching".to_owned();
        matching.matcher = PermissionRuleMatcher {
            tool_name: Some("shell".to_owned()),
            command_regex: Some(r"^git status(?:\s|$)".to_owned()),
            canonical_path_prefix: Some(Path::new("/workspace/repo").to_path_buf()),
        };
        let mut expired = rule(
            PermissionEffect::Deny,
            PermissionRuleScope::Request(request.request_id),
        );
        expired.expires_at = Some(now - Duration::seconds(1));

        let result = evaluate_permission(
            &request,
            VendorPermissionPolicy {
                admin_or_vendor_denied: false,
                explicitly_asks: false,
                default_decision: VendorDefaultDecision::Ask,
            },
            &[expired, matching],
            now,
        );
        assert_eq!(result.effect, Some(PermissionEffect::Allow));
        assert_eq!(
            result.source,
            PermissionEvaluationSource::MaestroRule("matching".to_owned())
        );
    }

    #[test]
    fn allow_path_rules_require_every_path_but_deny_rules_match_any_path() {
        let mut request = request();
        request.canonical_paths.push(PathBuf::from("/etc/hosts"));
        let matcher = PermissionRuleMatcher {
            canonical_path_prefix: Some(PathBuf::from("/workspace")),
            ..PermissionRuleMatcher::default()
        };
        let mut allow = rule(PermissionEffect::Allow, PermissionRuleScope::Global);
        allow.matcher = matcher.clone();
        let mut deny = rule(PermissionEffect::Deny, PermissionRuleScope::Global);
        deny.matcher = matcher;
        let vendor = VendorPermissionPolicy {
            admin_or_vendor_denied: false,
            explicitly_asks: false,
            default_decision: VendorDefaultDecision::Ask,
        };
        assert_eq!(
            evaluate_permission(&request, vendor, &[allow], Utc::now()).effect,
            None
        );
        assert_eq!(
            evaluate_permission(&request, vendor, &[deny], Utc::now()).effect,
            Some(PermissionEffect::Deny)
        );
    }

    #[test]
    fn persistent_rules_require_explicit_remember_and_stronger_confirmations() {
        let now = Utc::now();
        let draft = PermissionRuleDraft {
            id: "global-dangerous".to_owned(),
            effect: PermissionEffect::Allow,
            scope: PermissionRuleScope::Global,
            matcher: PermissionRuleMatcher::default(),
            expires_at: None,
            creation_source: "dialog".to_owned(),
            dangerous: true,
        };
        assert_eq!(
            create_persistent_permission_rule(
                draft.clone(),
                RuleCreationIntent::ThisRequestOnly,
                now,
            )
            .unwrap(),
            None
        );
        assert_eq!(
            create_persistent_permission_rule(
                draft.clone(),
                RuleCreationIntent::Remember {
                    global_confirmed: false,
                    dangerous_confirmed: true,
                },
                now,
            ),
            Err(PermissionRuleCreationError::GlobalConfirmationRequired)
        );
        assert_eq!(
            create_persistent_permission_rule(
                draft.clone(),
                RuleCreationIntent::Remember {
                    global_confirmed: true,
                    dangerous_confirmed: false,
                },
                now,
            ),
            Err(PermissionRuleCreationError::DangerousConfirmationRequired)
        );
        assert!(
            create_persistent_permission_rule(
                draft,
                RuleCreationIntent::Remember {
                    global_confirmed: true,
                    dangerous_confirmed: true,
                },
                now,
            )
            .unwrap()
            .is_some()
        );
    }
}
