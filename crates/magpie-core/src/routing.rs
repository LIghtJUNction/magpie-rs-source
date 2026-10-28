//! Bounded routing decisions over explicit account observations, without I/O.
//!
//! # Semantics
//!
//! Groups expand depth first in configured order. The requested group's strategy
//! applies to the resulting leaves; a child's strategy applies when that child
//! is requested directly. Hierarchical policy composition is not implemented.
//! Repeated provider/model/account leaves are rejected, including through a DAG,
//! rather than silently changing their weight. Group references must have weight
//! one; leaf weights default to one and accept 1..=1000.
//!
//! A configured model is not evidence of service, permission, budget or quota.
//! Missing/stale snapshots, unknown permission/budget, login failures, exhausted
//! quota and hard retry deadlines exclude a leaf. API/local accounts may explicitly
//! report `Quota::NotProvided`; subscriptions require a fresh upstream window.
//! Passing a reset time never replenishes a window without another observation.
//!
//! Plan-limited accounts are considered only after unrestricted accounts. Within
//! each class, normal members precede soft rate-limited members, then resting
//! members. Hard Retry-After deadlines are never bypassed. Soft sinking requires
//! positive observed quota; resting members need explicit last-resort permission.
//!
//! `order` picks the first eligible member. `rotate` moves from the previous
//! configured position. `weight` uses smooth weighted round robin, resetting its
//! scores when the eligible pool changes, so excluded members accrue no credit.
//! State advances once per successful selection, not on completion or inspection.
//! `smart` prefers subscriptions with the earliest observed reset; API/local
//! members follow in configured order. `usage` requires a common caller-selected
//! window/unit and fresh samples for the entire best-priority pool. `pace`
//! requires comparable weekly quota units for that pool and compares remaining
//! quota / seconds to reset, breaking ties by earliest reset. `fast` requires
//! fresh TTFT and throughput for that pool. With an output-token estimate it
//! compares TTFT + estimated generation duration; otherwise TTFT wins, then
//! throughput. Missing/incomparable ranking data falls back to configured order
//! with an explicit reason, never zero/infinite quota or invented speed.
//!
//! # Integration and remaining requirements
//!
//! This is a foundation for parts of REQ-040/041/042, not complete REQ acceptance.
//! Group CRUD/discovery/list visibility, provider/account policy composition,
//! multiple simultaneous quota windows/credits, provider fallback models and
//! atomic budget/concurrency reservations remain outside this module. REQ-043
//! rules/classifiers and REQ-044 session affinity/stays are not implemented.
//! REQ-045 transport retries, error classification, replay safety, cancellation,
//! timeout/backoff, tier negotiation and final Usage deduplication are not
//! implemented. A selection is neither an upstream attempt nor proof of success.
//!
//! Gateway should authenticate first, provide fresh credential-free snapshots,
//! reserve resources atomically, and dispatch exactly the selected target. A
//! safe retry needs request-local attempted-member limits and refreshed snapshots;
//! repeatedly calling `select` is not a retry loop. Storage should version group
//! configuration and observations separately, retaining last known quota on
//! refresh failure. Rebuilding a Router deliberately resets scheduling state.
//! Share one Router per configuration within the owning service, serialize its
//! selection calls, and revalidate revocation before dispatch.
//!
//! Only `std` is used. Bounds are safety limits, not performance promises.
//! No performance benchmark or full REQ acceptance is claimed here.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

/// Routing policy names accepted by configuration adapters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Strategy {
    /// Prefer the earliest fresh subscription quota reset.
    #[default]
    Smart,
    /// Preserve configured order after eligibility filtering.
    Order,
    /// Cycle over eligible configured positions.
    Rotate,
    /// Minimize measured usage in a common window and unit.
    Usage,
    /// Maximize remaining weekly quota per time until reset.
    Pace,
    /// Smooth weighted round robin with stable ties.
    Weight,
    /// Minimize measured latency or estimated completion duration.
    Fast,
}

impl FromStr for Strategy {
    type Err = RoutingError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "smart" => Ok(Self::Smart),
            "order" => Ok(Self::Order),
            "rotate" => Ok(Self::Rotate),
            "usage" => Ok(Self::Usage),
            "pace" => Ok(Self::Pace),
            "weight" => Ok(Self::Weight),
            "fast" | "fastest" | "speed" => Ok(Self::Fast),
            _ => Err(RoutingError::UnknownStrategy),
        }
    }
}

/// Distinguishes chat models from models used only for decisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Purpose {
    /// May be used for normal Agent chat.
    #[default]
    Chat,
    /// May only be used for a decision request.
    Decision,
}

/// Stable public identity; account is an opaque ID, never a credential or email.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Target {
    /// Provider namespace, excluding the reserved namespace `group`.
    pub provider: String,
    /// Upstream model ID, allowing slash-separated model namespaces.
    pub model: String,
    /// Stable account/key ID, also for a keyless local service.
    pub account: String,
}

impl Target {
    /// Returns the public model ID without revealing an account identifier.
    #[must_use]
    pub fn model_id(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    fn validate(&self) -> Result<(), RoutingError> {
        identifier(&self.provider, "provider")?;
        identifier(&self.account, "account")?;
        if self.provider == "group"
            || self.model.len() > 256
            || !self
                .model
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !self
                .model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-:@/".contains(&byte))
            || self.model.split('/').any(|s| matches!(s, "" | "." | ".."))
        {
            return Err(RoutingError::InvalidIdentifier("provider/model"));
        }
        Ok(())
    }
}

/// An explicitly configured group member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberTarget {
    /// An account/model leaf.
    Model(Target),
    /// Another group's bare ID, without the `group/` prefix.
    Group(String),
}

/// Ordered membership, including user disablement and optional weight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Leaf identity or nested group reference.
    pub target: MemberTarget,
    /// Disabled members remain in explanations.
    pub enabled: bool,
    /// Defaults to one; explicit values must be 1..=1000.
    pub weight: Option<u16>,
}

impl Member {
    /// Creates an enabled leaf with implicit weight one.
    #[must_use]
    pub fn model(target: Target) -> Self {
        Self {
            target: MemberTarget::Model(target),
            enabled: true,
            weight: None,
        }
    }

    /// Creates an enabled structural reference with implicit weight one.
    #[must_use]
    pub fn group(id: &str) -> Self {
        Self {
            target: MemberTarget::Group(id.to_owned()),
            enabled: true,
            weight: None,
        }
    }
}

/// A configuration value; validated and frozen by `Router::new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Bare group ID; requests use `group/<id>`.
    pub id: String,
    /// Chat and decision groups cannot reference each other.
    pub purpose: Purpose,
    /// Policy for this group when requested directly.
    pub strategy: Strategy,
    /// Disabling a group disables all its expanded leaves.
    pub enabled: bool,
    /// User order is retained without sorting.
    pub members: Vec<Member>,
}

/// Configurable safety limits, each bounded above by its default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum groups, at most 128.
    pub groups: usize,
    /// Maximum declared members across all groups, at most 2048.
    pub members: usize,
    /// Maximum total expanded leaves across all groups, at most 4096.
    pub expanded_members: usize,
    /// Maximum groups on an active expansion path, root included, at most 32.
    pub depth: usize,
    /// Maximum total visited group/member nodes during construction, at most 16384.
    pub expansion_steps: usize,
    /// Maximum snapshots in one selection call, at most 4096.
    pub snapshots: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            groups: 128,
            members: 2048,
            expanded_members: 4096,
            depth: 32,
            expansion_steps: 16384,
            snapshots: 4096,
        }
    }
}

/// Timestamped upstream/local observation; timestamps are Unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation<T> {
    /// Actual observed value, without synthesized defaults.
    pub value: T,
    /// Successful observation time.
    pub observed_at: u64,
    /// Exclusive expiry; callers choose their actual freshness policy.
    pub valid_until: u64,
}

impl<T> Observation<T> {
    fn fresh(&self, now: u64) -> bool {
        self.observed_at <= now && now < self.valid_until
    }
}

/// The kind of account, independently of catalog membership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountKind {
    /// A subscription requires observed quota.
    Subscription,
    /// A normal API key may explicitly have no quota endpoint.
    ApiKey,
    /// A keyless/local service may explicitly have no quota endpoint.
    Local,
}

/// Confirmed service state from the account owner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServiceState {
    /// No usable evidence of service readiness.
    #[default]
    Unknown,
    /// Ready to attempt a request; does not guarantee an upstream response.
    Ready,
    /// User paused the account.
    Paused,
    /// Provider/account is disabled.
    Disabled,
    /// Login or authorization must be renewed.
    NeedsLogin,
    /// The model is explicitly unsupported.
    UnsupportedModel,
    /// Transport/plugin/client is known unavailable.
    Unavailable,
}

/// Caller-evaluated budget/cap state, without estimating unknown prices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Budget {
    /// Budget cannot be established; selection is denied.
    #[default]
    Unknown,
    /// No user budget/cap applies.
    Unrestricted,
    /// All applicable caps and budgets permit this request.
    WithinLimit,
    /// A cap or budget has been reached.
    Exceeded,
}

/// Caller-evaluated model permission and plan restriction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ModelAccess {
    /// Authorization cannot be established.
    #[default]
    Unknown,
    /// Model is permitted without a plan restriction.
    Allowed,
    /// Only eligible when no unrestricted account can serve.
    PlanLimited,
    /// Model must not be served.
    Denied,
}

/// Units that must match before quota/usage values are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Number of requests.
    Requests,
    /// Number of tokens.
    Tokens,
    /// A fraction expressed as 0..=10000 basis points.
    BasisPoints,
}

/// Actual upstream quota period, without assuming a duration from its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaPeriod {
    /// A weekly window explicitly reported by the owner.
    Weekly,
    /// Any other supported upstream window.
    Other,
}

/// A single upstream quota window; multi-window intersection belongs to the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaWindow {
    /// Remaining quota in `unit`, including an actual zero.
    pub remaining: u64,
    /// Unit of remaining quota.
    pub unit: Unit,
    /// Actual upstream reset time, Unix seconds.
    pub resets_at: u64,
    /// Whether this is explicitly a weekly window.
    pub period: QuotaPeriod,
}

/// Unknown, unprovided and observed quota are different states.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Quota {
    /// Reading failed or quota is not yet known; never means unlimited.
    #[default]
    Unknown,
    /// Explicitly no upstream quota endpoint, only for API/local accounts.
    NotProvided,
    /// Last successfully read window, with its original timestamps.
    Observed(Observation<QuotaWindow>),
}

/// Exact common interval/unit for comparable usage measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageWindow {
    /// Inclusive start, Unix seconds.
    pub starts_at: u64,
    /// Exclusive end, Unix seconds.
    pub ends_at: u64,
    /// Unit of each compared measurement.
    pub unit: Unit,
}

/// Measured recent or current-period usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// Measurement window shared by all compared candidates.
    pub window: UsageWindow,
    /// Actual measured use; absence is represented by no observation.
    pub used: u64,
}

/// Recent first-token latency and generation throughput, both measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Speed {
    /// Time to first token in milliseconds; a measured zero is permitted.
    pub first_token_ms: u32,
    /// Tokens per second times 1000; zero is invalid for ranking.
    pub milli_tokens_per_second: u32,
}

/// A credential-free, request-specific service snapshot supplied by the owner.
///
/// The outer validity covers readiness, authorization, budget and retry state.
/// Usage/speed/quota have separate validity. Constructing this value does not
/// reserve resources; Gateway must check and reserve them before dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Exact leaf identity, not merely a public model/catalog ID.
    pub target: Target,
    /// Account transport/authorization kind.
    pub kind: AccountKind,
    /// Successful snapshot time.
    pub observed_at: u64,
    /// Exclusive expiry of the outer snapshot.
    pub valid_until: u64,
    /// Actual service readiness.
    pub service: ServiceState,
    /// Effective model permission.
    pub access: ModelAccess,
    /// Aggregate effective cap/budget state.
    pub budget: Budget,
    /// Quota state; not inferred from account kind.
    pub quota: Quota,
    /// Recent/current usage, if measured.
    pub usage: Option<Observation<Usage>>,
    /// Recent speed, if measured.
    pub speed: Option<Observation<Speed>>,
    /// Hard retry deadline, including actual Retry-After; never bypassed.
    pub retry_not_before: Option<u64>,
    /// Soft 429 sinking deadline, distinct from hard Retry-After.
    pub rate_limited_until: Option<u64>,
    /// Soft resting deadline, eligible only under an explicit last-resort policy.
    pub resting_until: Option<u64>,
}

/// Whether soft rate-limited members with observed quota can be a last choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RateLimitPolicy {
    /// Skip while the soft deadline is active.
    #[default]
    Skip,
    /// Place behind normal candidates when positive quota is confirmed.
    Sink,
}

/// Whether soft resting members may be selected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RestingPolicy {
    /// Skip until the resting deadline.
    #[default]
    Skip,
    /// Consider after normal and soft rate-limited candidates.
    LastResort,
}

/// Explicit decision context; no prompt or implicit session inference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Context {
    /// Current Unix seconds supplied by the owner.
    pub now: u64,
    /// Chat or explicit decision endpoint.
    pub purpose: Purpose,
    /// Required common usage window for the usage strategy.
    pub usage_window: Option<UsageWindow>,
    /// Caller estimate for fast ranking; absence uses TTFT then throughput.
    pub expected_output_tokens: Option<u32>,
    /// Handling of soft rate limits.
    pub rate_limit_policy: RateLimitPolicy,
    /// Handling of soft resting members.
    pub resting_policy: RestingPolicy,
}

/// Effective leaf after ordered structural group expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedMember {
    /// Exact upstream destination.
    pub target: Target,
    /// Validated effective leaf weight.
    pub weight: u16,
    /// Includes disablement of every ancestor/reference.
    pub enabled: bool,
}

/// First decisive reason a configured member cannot currently be attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exclusion {
    /// Configuration disabled the leaf or an ancestor.
    Disabled,
    /// No service snapshot was supplied.
    MissingSnapshot,
    /// Snapshot is expired, has future observation time or invalid validity.
    StaleSnapshot,
    /// Account owner reports a non-ready state.
    Service(ServiceState),
    /// Unknown or denied model permission.
    ModelAccess(ModelAccess),
    /// Unknown or exhausted user budget/cap.
    Budget(Budget),
    /// Quota was unknown, or unprovided for a subscription.
    UnknownQuota,
    /// Quota observation is expired, future-dated or its reset has passed.
    StaleQuota,
    /// A quota value has an invalid unit/value combination.
    InvalidQuota,
    /// Actually observed remaining quota is zero.
    ExhaustedQuota,
    /// A hard retry deadline is still active.
    RetryNotBefore(u64),
    /// Soft rate-limited, without explicit permitted sinking and known quota.
    RateLimited(u64),
    /// Resting, without last-resort permission.
    Resting(u64),
}

/// Priority before any ranking strategy, in lexicographic field order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority {
    /// False for unrestricted access; true only for a plan-limited model.
    pub plan_limited: bool,
    /// 0 normal, 1 soft rate-limited, 2 resting.
    pub recovery_stage: u8,
}

/// Ordered explanation for one expanded leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Leaf identity in original configured order.
    pub target: Target,
    /// Eligible priority, or the first decisive exclusion.
    pub eligibility: Result<Priority, Exclusion>,
}

/// Why a metric strategy used configured order for its best-priority pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// Caller supplied no common usage window.
    MissingUsageWindow,
    /// A usage observation is missing, expired or from another window/unit.
    IncompleteUsage,
    /// A weekly quota observation is absent or has incomparable units.
    IncomparableWeeklyQuota,
    /// A speed observation is absent, expired or has zero throughput.
    IncompleteSpeed,
    /// Smart had no subscription with a fresh quota window.
    NoSubscriptionQuota,
}

/// Actual basis used for the chosen member, retaining known metric values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionReason {
    /// First eligible configured position, optionally after metric fallback.
    ConfiguredOrder,
    /// Next eligible configured position.
    RoundRobin,
    /// Smooth weighted selection with this configured weight.
    SmoothWeight(u16),
    /// Earliest observed subscription reset.
    SubscriptionReset(u64),
    /// Least measured use in the supplied common window.
    Usage(Usage),
    /// Exact pace ratio; no floating-point approximation is used for comparison.
    WeeklyPace {
        /// Actual remaining quota.
        remaining: u64,
        /// Positive duration until the actual reset.
        seconds_to_reset: u64,
        /// Comparable quota unit.
        unit: Unit,
    },
    /// Measured TTFT, breaking equal latency ties by higher throughput.
    LatencyThenThroughput(Speed),
    /// Estimated completion based on real speed and caller-supplied output size.
    EstimatedCompletion {
        /// Observed TTFT and throughput.
        speed: Speed,
        /// Caller estimate, not a measured output length.
        output_tokens: u32,
    },
}

/// One selected destination, not an attempted or completed upstream request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Exact upstream destination.
    pub target: Target,
    /// Priority class used before strategy ranking.
    pub priority: Priority,
    /// Explainable choice with actual metric inputs.
    pub reason: SelectionReason,
}

/// Bounded decision and all configured member exclusions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// Requested policy, even when insufficient data caused an order fallback.
    pub strategy: Strategy,
    /// None means no member can currently serve; inspect `candidates`.
    pub selected: Option<Selection>,
    /// Present only when ranking fell back to configured order.
    pub fallback: Option<Fallback>,
    /// Remains in expanded configuration order, never ranked/sorted in place.
    pub candidates: Vec<Candidate>,
}

/// Configuration/selection input errors, without account secrets or body data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingError {
    /// Limits must be positive and no larger than the documented ceilings.
    InvalidLimits,
    /// A named collection/work bound was exceeded.
    LimitExceeded(&'static str),
    /// A public identifier is malformed or too long; input is not echoed.
    InvalidIdentifier(&'static str),
    /// Unknown policy spelling; input is not echoed.
    UnknownStrategy,
    /// Two groups declared the same ID.
    DuplicateGroup(String),
    /// Requested/referenced group does not exist.
    UnknownGroup(String),
    /// A group has no declared members.
    EmptyGroup(String),
    /// Active reference path including the closing group.
    Cycle(Vec<String>),
    /// Reference path exceeds the configured depth.
    DepthLimit(String),
    /// Expanded members contain the same provider/model/account twice.
    DuplicateTarget(String),
    /// Leaf/reference weight is outside 1..=1000.
    InvalidWeight(String),
    /// Hierarchical group weights are not supported.
    NestedWeight(String),
    /// Chat/decision groups cannot be mixed.
    PurposeMismatch(String),
    /// More than one observation was supplied for the same leaf.
    DuplicateSnapshot,
    /// Invalid caller usage interval or one that has not started.
    InvalidUsageWindow,
    /// This Router has already seen a later `now` value.
    ClockMovedBackwards,
    /// Scheduler arithmetic exceeded its representable range.
    StateOverflow,
}

impl fmt::Display for RoutingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => {
                f.write_str("routing limits must be positive and within ceilings")
            }
            Self::LimitExceeded(name) => write!(f, "routing {name} limit exceeded"),
            Self::InvalidIdentifier(field) => write!(f, "invalid routing {field} identifier"),
            Self::UnknownStrategy => f.write_str("unknown routing strategy"),
            Self::DuplicateGroup(id) => write!(f, "duplicate routing group '{id}'"),
            Self::UnknownGroup(id) => write!(f, "routing group '{id}' does not exist"),
            Self::EmptyGroup(id) => write!(f, "routing group '{id}' is empty"),
            Self::Cycle(path) => write!(f, "routing group cycle: {}", path.join(" -> ")),
            Self::DepthLimit(id) => write!(f, "routing depth limit reached at group '{id}'"),
            Self::DuplicateTarget(id) => write!(f, "routing group '{id}' repeats a target"),
            Self::InvalidWeight(id) => write!(f, "routing group '{id}' weight must be 1..=1000"),
            Self::NestedWeight(id) => write!(f, "routing group '{id}' references need weight one"),
            Self::PurposeMismatch(id) => write!(f, "routing group '{id}' has incompatible purpose"),
            Self::DuplicateSnapshot => f.write_str("duplicate routing target snapshot"),
            Self::InvalidUsageWindow => f.write_str("invalid routing usage window"),
            Self::ClockMovedBackwards => f.write_str("routing clock moved backwards"),
            Self::StateOverflow => f.write_str("routing scheduler state overflow"),
        }
    }
}

impl std::error::Error for RoutingError {}

#[derive(Debug, Default)]
struct Scheduler {
    last_selected: Option<usize>,
    pool: Vec<usize>,
    scores: Vec<i64>,
}

/// Immutable bounded configuration plus bounded per-group scheduling state.
///
/// Construction validates even disabled/unrequested groups. State and expanded
/// leaves have at most `limits.expanded_members` slots in total. Selection uses
/// temporary bounded snapshot indexes and reports; it retains no observation,
/// session, request body, attempt history, timestamp series or arbitrary ID.
/// Snapshot indexing is O(S log S), eligibility O(N log S), ranking O(N).
/// These are algorithmic bounds, not measured latency/memory acceptance.
#[derive(Debug)]
pub struct Router {
    groups: Vec<Group>,
    index: BTreeMap<String, usize>,
    expanded: Vec<Vec<ExpandedMember>>,
    schedulers: Vec<Scheduler>,
    limits: Limits,
    last_now: Option<u64>,
}

impl Router {
    /// Validates all groups and compiles bounded, ordered structural expansions.
    ///
    /// # Errors
    ///
    /// Rejects malformed limits/identities, duplicate/empty/missing groups, cycles,
    /// excessive depth/work/expansion, duplicate leaves, invalid weights,
    /// hierarchical weights and mixed chat/decision references. No I/O occurs.
    pub fn new(groups: Vec<Group>, limits: Limits) -> Result<Self, RoutingError> {
        validate_limits(limits)?;
        if groups.len() > limits.groups {
            return Err(RoutingError::LimitExceeded("groups"));
        }
        let mut declared = 0;
        let mut index = BTreeMap::new();
        for (position, group) in groups.iter().enumerate() {
            identifier(&group.id, "group")?;
            if index.insert(group.id.clone(), position).is_some() {
                return Err(RoutingError::DuplicateGroup(group.id.clone()));
            }
            if group.members.is_empty() {
                return Err(RoutingError::EmptyGroup(group.id.clone()));
            }
            if group.members.len() > limits.members - declared {
                return Err(RoutingError::LimitExceeded("declared members"));
            }
            declared += group.members.len();
            for member in &group.members {
                if !(1..=1000).contains(&member.weight.unwrap_or(1)) {
                    return Err(RoutingError::InvalidWeight(group.id.clone()));
                }
                match &member.target {
                    MemberTarget::Model(target) => target.validate()?,
                    MemberTarget::Group(id) => {
                        identifier(id, "group reference")?;
                        if member.weight.unwrap_or(1) != 1 {
                            return Err(RoutingError::NestedWeight(group.id.clone()));
                        }
                    }
                }
            }
        }
        let mut expansion = Expansion {
            groups: &groups,
            index: &index,
            limits,
            steps: 0,
            leaves: 0,
            path: Vec::new(),
        };
        let mut expanded = Vec::with_capacity(groups.len());
        for (root, group) in groups.iter().enumerate() {
            let mut output = Vec::new();
            expansion.walk(root, true, &mut output)?;
            let mut targets = BTreeSet::new();
            for member in &output {
                if !targets.insert(&member.target) {
                    return Err(RoutingError::DuplicateTarget(group.id.clone()));
                }
            }
            expanded.push(output);
        }
        let schedulers = expanded
            .iter()
            .map(|members| Scheduler {
                scores: vec![0; members.len()],
                ..Scheduler::default()
            })
            .collect();
        Ok(Self {
            groups,
            index,
            expanded,
            schedulers,
            limits,
            last_now: None,
        })
    }

    /// Returns immutable validated group configuration in user order.
    #[must_use]
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Returns a structural expansion without advancing scheduler state.
    ///
    /// # Errors
    ///
    /// Rejects malformed or missing `group/<id>` references.
    pub fn expanded_members(&self, group: &str) -> Result<&[ExpandedMember], RoutingError> {
        Ok(&self.expanded[self.group_index(group)?])
    }

    /// Resets all round-robin/weight state and the caller clock guard.
    pub fn reset(&mut self) {
        for scheduler in &mut self.schedulers {
            scheduler.last_selected = None;
            scheduler.pool.clear();
            scheduler.scores.fill(0);
        }
        self.last_now = None;
    }

    /// Selects once from actual observations, retaining original report order.
    ///
    /// Every successful selection consumes a scheduling turn. A no-service
    /// decision consumes no turn and clears weight credit. The caller must not
    /// use this mutating method as a read-only UI preview.
    ///
    /// # Errors
    ///
    /// Rejects invalid/missing group IDs, purpose mismatch, excessive or duplicate
    /// snapshots, invalid target IDs/usage context, regressing caller time and
    /// scheduler overflow. Input validation errors do not advance state.
    pub fn select(
        &mut self,
        group: &str,
        snapshots: &[Snapshot],
        context: Context,
    ) -> Result<Decision, RoutingError> {
        let position = self.group_index(group)?;
        let configured = &self.groups[position];
        if configured.purpose != context.purpose {
            return Err(RoutingError::PurposeMismatch(configured.id.clone()));
        }
        if self.last_now.is_some_and(|last| context.now < last) {
            return Err(RoutingError::ClockMovedBackwards);
        }
        if let Some(window) = context.usage_window {
            if window.starts_at >= window.ends_at || window.starts_at > context.now {
                return Err(RoutingError::InvalidUsageWindow);
            }
        }
        if snapshots.len() > self.limits.snapshots {
            return Err(RoutingError::LimitExceeded("snapshots"));
        }
        let mut observations = BTreeMap::new();
        for snapshot in snapshots {
            snapshot.target.validate()?;
            if observations.insert(&snapshot.target, snapshot).is_some() {
                return Err(RoutingError::DuplicateSnapshot);
            }
        }
        let members = &self.expanded[position];
        let candidates: Vec<_> = members
            .iter()
            .map(|member| Candidate {
                target: member.target.clone(),
                eligibility: eligibility(
                    member,
                    observations.get(&member.target).copied(),
                    context,
                ),
            })
            .collect();
        let best_priority = candidates.iter().filter_map(|c| c.eligibility.ok()).min();
        let pool: Vec<_> = candidates
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match c.eligibility {
                Ok(priority) if Some(priority) == best_priority => Some(i),
                _ => None,
            })
            .collect();
        let scheduler = &mut self.schedulers[position];
        if scheduler.pool != pool {
            scheduler.scores.fill(0);
            scheduler.pool.clone_from(&pool);
        }
        let mut decision = Decision {
            strategy: configured.strategy,
            selected: None,
            fallback: None,
            candidates,
        };
        let Some(priority) = best_priority else {
            self.last_now = Some(context.now);
            return Ok(decision);
        };
        let data: Vec<_> = pool
            .iter()
            .filter_map(|&i| observations.get(&members[i].target).copied())
            .collect();
        let (chosen, reason, fallback) = choose(
            configured.strategy,
            &pool,
            members,
            &data,
            scheduler,
            context,
        )?;
        scheduler.last_selected = Some(chosen);
        self.last_now = Some(context.now);
        decision.selected = Some(Selection {
            target: members[chosen].target.clone(),
            priority,
            reason,
        });
        decision.fallback = fallback;
        Ok(decision)
    }

    fn group_index(&self, reference: &str) -> Result<usize, RoutingError> {
        let id = reference
            .strip_prefix("group/")
            .ok_or(RoutingError::InvalidIdentifier("group reference"))?;
        identifier(id, "group reference")?;
        self.index
            .get(id)
            .copied()
            .ok_or_else(|| RoutingError::UnknownGroup(id.to_owned()))
    }
}

struct Expansion<'config> {
    groups: &'config [Group],
    index: &'config BTreeMap<String, usize>,
    limits: Limits,
    steps: usize,
    leaves: usize,
    path: Vec<usize>,
}

impl Expansion<'_> {
    fn step(&mut self) -> Result<(), RoutingError> {
        if self.steps == self.limits.expansion_steps {
            return Err(RoutingError::LimitExceeded("expansion steps"));
        }
        self.steps += 1;
        Ok(())
    }

    fn walk(
        &mut self,
        position: usize,
        enabled: bool,
        output: &mut Vec<ExpandedMember>,
    ) -> Result<(), RoutingError> {
        self.step()?;
        let groups = self.groups;
        let group = &groups[position];
        if self.path.contains(&position) {
            let mut path: Vec<_> = self
                .path
                .iter()
                .map(|&i| self.groups[i].id.clone())
                .collect();
            path.push(group.id.clone());
            return Err(RoutingError::Cycle(path));
        }
        if self.path.len() == self.limits.depth {
            return Err(RoutingError::DepthLimit(group.id.clone()));
        }
        self.path.push(position);
        for member in &group.members {
            self.step()?;
            let enabled = enabled && group.enabled && member.enabled;
            match &member.target {
                MemberTarget::Model(target) => {
                    if self.leaves == self.limits.expanded_members {
                        return Err(RoutingError::LimitExceeded("expanded members"));
                    }
                    self.leaves += 1;
                    output.push(ExpandedMember {
                        target: target.clone(),
                        weight: member.weight.unwrap_or(1),
                        enabled,
                    });
                }
                MemberTarget::Group(id) => {
                    let child = *self
                        .index
                        .get(id)
                        .ok_or_else(|| RoutingError::UnknownGroup(id.clone()))?;
                    if self.groups[child].purpose != group.purpose {
                        return Err(RoutingError::PurposeMismatch(id.clone()));
                    }
                    self.walk(child, enabled, output)?;
                }
            }
        }
        self.path.pop();
        Ok(())
    }
}

fn identifier(value: &str, field: &'static str) -> Result<(), RoutingError> {
    if value.len() > 128
        || !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(RoutingError::InvalidIdentifier(field));
    }
    Ok(())
}

fn validate_limits(limits: Limits) -> Result<(), RoutingError> {
    let ceiling = Limits::default();
    for (value, maximum) in [
        (limits.groups, ceiling.groups),
        (limits.members, ceiling.members),
        (limits.expanded_members, ceiling.expanded_members),
        (limits.depth, ceiling.depth),
        (limits.expansion_steps, ceiling.expansion_steps),
        (limits.snapshots, ceiling.snapshots),
    ] {
        if value == 0 || value > maximum {
            return Err(RoutingError::InvalidLimits);
        }
    }
    Ok(())
}

fn eligibility(
    member: &ExpandedMember,
    snapshot: Option<&Snapshot>,
    context: Context,
) -> Result<Priority, Exclusion> {
    if !member.enabled {
        return Err(Exclusion::Disabled);
    }
    let snapshot = snapshot.ok_or(Exclusion::MissingSnapshot)?;
    if snapshot.observed_at > context.now || context.now >= snapshot.valid_until {
        return Err(Exclusion::StaleSnapshot);
    }
    if snapshot.service != ServiceState::Ready {
        return Err(Exclusion::Service(snapshot.service));
    }
    if !matches!(
        snapshot.access,
        ModelAccess::Allowed | ModelAccess::PlanLimited
    ) {
        return Err(Exclusion::ModelAccess(snapshot.access));
    }
    if !matches!(snapshot.budget, Budget::Unrestricted | Budget::WithinLimit) {
        return Err(Exclusion::Budget(snapshot.budget));
    }
    let known_quota = match snapshot.quota {
        Quota::Unknown => return Err(Exclusion::UnknownQuota),
        Quota::NotProvided if snapshot.kind == AccountKind::Subscription => {
            return Err(Exclusion::UnknownQuota);
        }
        Quota::NotProvided => false,
        Quota::Observed(observation) => {
            let window = observation.value;
            if !observation.fresh(context.now) || window.resets_at <= context.now {
                return Err(Exclusion::StaleQuota);
            }
            if window.unit == Unit::BasisPoints && window.remaining > 10_000 {
                return Err(Exclusion::InvalidQuota);
            }
            if window.remaining == 0 {
                return Err(Exclusion::ExhaustedQuota);
            }
            true
        }
    };
    if let Some(until) = snapshot
        .retry_not_before
        .filter(|&until| until > context.now)
    {
        return Err(Exclusion::RetryNotBefore(until));
    }
    let mut recovery_stage = 0;
    if let Some(until) = snapshot
        .rate_limited_until
        .filter(|&until| until > context.now)
    {
        if context.rate_limit_policy != RateLimitPolicy::Sink || !known_quota {
            return Err(Exclusion::RateLimited(until));
        }
        recovery_stage = 1;
    }
    if let Some(until) = snapshot.resting_until.filter(|&until| until > context.now) {
        if context.resting_policy != RestingPolicy::LastResort {
            return Err(Exclusion::Resting(until));
        }
        recovery_stage = 2;
    }
    Ok(Priority {
        plan_limited: snapshot.access == ModelAccess::PlanLimited,
        recovery_stage,
    })
}

fn choose(
    strategy: Strategy,
    pool: &[usize],
    members: &[ExpandedMember],
    data: &[&Snapshot],
    scheduler: &mut Scheduler,
    context: Context,
) -> Result<(usize, SelectionReason, Option<Fallback>), RoutingError> {
    let order = |fallback| (pool[0], SelectionReason::ConfiguredOrder, fallback);
    match strategy {
        Strategy::Order => Ok(order(None)),
        Strategy::Rotate => {
            let chosen = scheduler
                .last_selected
                .and_then(|last| pool.iter().copied().find(|&i| i > last))
                .unwrap_or(pool[0]);
            Ok((chosen, SelectionReason::RoundRobin, None))
        }
        Strategy::Weight => {
            // Compute into bounded temporary state so arithmetic failure is atomic.
            let mut scores = scheduler.scores.clone();
            let total: i64 = pool.iter().map(|&i| i64::from(members[i].weight)).sum();
            let mut chosen = pool[0];
            for &i in pool {
                scores[i] = scores[i]
                    .checked_add(i64::from(members[i].weight))
                    .ok_or(RoutingError::StateOverflow)?;
                if scores[i] > scores[chosen] {
                    chosen = i;
                }
            }
            scores[chosen] = scores[chosen]
                .checked_sub(total)
                .ok_or(RoutingError::StateOverflow)?;
            scheduler.scores = scores;
            Ok((
                chosen,
                SelectionReason::SmoothWeight(members[chosen].weight),
                None,
            ))
        }
        Strategy::Smart => {
            let earliest = data
                .iter()
                .enumerate()
                .filter_map(|(i, snapshot)| match (snapshot.kind, snapshot.quota) {
                    (AccountKind::Subscription, Quota::Observed(observation)) => {
                        Some((i, observation.value.resets_at))
                    }
                    _ => None,
                })
                .min_by_key(|&(_, reset)| reset);
            Ok(match earliest {
                Some((i, reset)) => (pool[i], SelectionReason::SubscriptionReset(reset), None),
                None => order(Some(Fallback::NoSubscriptionQuota)),
            })
        }
        Strategy::Usage => {
            let Some(window) = context.usage_window else {
                return Ok(order(Some(Fallback::MissingUsageWindow)));
            };
            let samples: Option<Vec<_>> = data
                .iter()
                .map(|snapshot| {
                    snapshot
                        .usage
                        .filter(|o| {
                            o.fresh(context.now)
                                && o.observed_at >= window.starts_at
                                && o.value.window == window
                                && (window.unit != Unit::BasisPoints || o.value.used <= 10_000)
                        })
                        .map(|o| o.value)
                })
                .collect();
            let Some(samples) = samples else {
                return Ok(order(Some(Fallback::IncompleteUsage)));
            };
            let i = best_index(&samples, |a, b| a.used.cmp(&b.used));
            Ok((pool[i], SelectionReason::Usage(samples[i]), None))
        }
        Strategy::Pace => {
            let samples: Option<Vec<_>> = data
                .iter()
                .map(|snapshot| match snapshot.quota {
                    Quota::Observed(o) if o.value.period == QuotaPeriod::Weekly => Some(o.value),
                    _ => None,
                })
                .collect();
            let Some(samples) = samples else {
                return Ok(order(Some(Fallback::IncomparableWeeklyQuota)));
            };
            if samples.iter().any(|s| s.unit != samples[0].unit) {
                return Ok(order(Some(Fallback::IncomparableWeeklyQuota)));
            }
            let i = best_index(&samples, |a, b| {
                // Cross products fit u128 for any pair of u64 inputs.
                let left = u128::from(a.remaining) * u128::from(b.resets_at - context.now);
                let right = u128::from(b.remaining) * u128::from(a.resets_at - context.now);
                right.cmp(&left).then(a.resets_at.cmp(&b.resets_at))
            });
            Ok((
                pool[i],
                SelectionReason::WeeklyPace {
                    remaining: samples[i].remaining,
                    seconds_to_reset: samples[i].resets_at - context.now,
                    unit: samples[i].unit,
                },
                None,
            ))
        }
        Strategy::Fast => {
            let samples: Option<Vec<_>> = data
                .iter()
                .map(|snapshot| {
                    snapshot
                        .speed
                        .filter(|o| o.fresh(context.now) && o.value.milli_tokens_per_second > 0)
                        .map(|o| o.value)
                })
                .collect();
            let Some(samples) = samples else {
                return Ok(order(Some(Fallback::IncompleteSpeed)));
            };
            let i = best_index(&samples, |a, b| {
                if let Some(tokens) = context.expected_output_tokens {
                    let numerator = |speed: &Speed| {
                        u128::from(speed.first_token_ms) * u128::from(speed.milli_tokens_per_second)
                            + u128::from(tokens) * 1_000_000
                    };
                    let left = numerator(a) * u128::from(b.milli_tokens_per_second);
                    let right = numerator(b) * u128::from(a.milli_tokens_per_second);
                    left.cmp(&right)
                } else {
                    a.first_token_ms
                        .cmp(&b.first_token_ms)
                        .then(b.milli_tokens_per_second.cmp(&a.milli_tokens_per_second))
                }
            });
            let reason = match context.expected_output_tokens {
                Some(output_tokens) => SelectionReason::EstimatedCompletion {
                    speed: samples[i],
                    output_tokens,
                },
                None => SelectionReason::LatencyThenThroughput(samples[i]),
            };
            Ok((pool[i], reason, None))
        }
    }
}

fn best_index<T>(samples: &[T], compare: impl Fn(&T, &T) -> Ordering) -> usize {
    let mut best = 0;
    for i in 1..samples.len() {
        if compare(&samples[i], &samples[best]) == Ordering::Less {
            best = i;
        }
    }
    best
}
