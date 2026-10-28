//! Synthetic public-behavior cases for the isolated routing foundation.
//!
//! These cases do not prove Gateway retries, persistence, account compatibility,
//! full REQ-040..045 acceptance or performance.

use magpie_core::routing::{
    AccountKind, Budget, Candidate, Context, Exclusion, Fallback, Group, Limits, Member,
    MemberTarget, ModelAccess, Observation, Purpose, Quota, QuotaPeriod, QuotaWindow,
    RateLimitPolicy, RestingPolicy, Router, RoutingError, SelectionReason, ServiceState, Snapshot,
    Speed, Strategy, Target, Unit, Usage, UsageWindow,
};

const NOW: u64 = 1_000;

fn target(account: &str) -> Target {
    Target {
        provider: "fixture".to_owned(),
        model: "coding".to_owned(),
        account: account.to_owned(),
    }
}

fn group(id: &str, strategy: Strategy, accounts: &[&str]) -> Group {
    Group {
        id: id.to_owned(),
        purpose: Purpose::Chat,
        strategy,
        enabled: true,
        members: accounts
            .iter()
            .map(|id| Member::model(target(id)))
            .collect(),
    }
}

fn router(strategy: Strategy, accounts: &[&str]) -> Router {
    Router::new(vec![group("main", strategy, accounts)], Limits::default())
        .expect("synthetic routing configuration is valid")
}

fn context() -> Context {
    Context {
        now: NOW,
        ..Context::default()
    }
}

fn observed<T>(value: T) -> Observation<T> {
    Observation {
        value,
        observed_at: NOW - 10,
        valid_until: NOW + 100,
    }
}

fn ready(account: &str) -> Snapshot {
    Snapshot {
        target: target(account),
        kind: AccountKind::ApiKey,
        observed_at: NOW - 10,
        valid_until: NOW + 100,
        service: ServiceState::Ready,
        access: ModelAccess::Allowed,
        budget: Budget::Unrestricted,
        quota: Quota::NotProvided,
        usage: None,
        speed: None,
        retry_not_before: None,
        rate_limited_until: None,
        resting_until: None,
    }
}

fn subscription(account: &str, remaining: u64, resets_at: u64) -> Snapshot {
    Snapshot {
        kind: AccountKind::Subscription,
        quota: Quota::Observed(observed(QuotaWindow {
            remaining,
            unit: Unit::Tokens,
            resets_at,
            period: QuotaPeriod::Weekly,
        })),
        ..ready(account)
    }
}

fn account(router: &mut Router, snapshots: &[Snapshot], context: Context) -> Option<String> {
    router
        .select("group/main", snapshots, context)
        .expect("synthetic request context is valid")
        .selected
        .map(|selection| selection.target.account)
}

fn rejection(candidate: &Candidate) -> Exclusion {
    candidate
        .eligibility
        .expect_err("this candidate must be excluded")
}

#[test]
fn order_uses_user_order_and_only_changes_after_observed_unavailability() {
    let mut router = router(Strategy::Order, &["z-first", "a-second"]);
    let mut snapshots = [ready("a-second"), ready("z-first")];
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("z-first"),
        "snapshot/key ordering must not replace user ordering"
    );
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("z-first")
    );
    snapshots[1].retry_not_before = Some(NOW + 5);
    let decision = router
        .select("group/main", &snapshots, context())
        .expect("observe a hard retry deadline");
    assert_eq!(
        decision
            .selected
            .expect("second member can serve")
            .target
            .account,
        "a-second"
    );
    assert_eq!(
        rejection(&decision.candidates[0]),
        Exclusion::RetryNotBefore(NOW + 5)
    );
    assert_eq!(decision.candidates[0].target.account, "z-first");
    assert_eq!(decision.candidates[1].target.account, "a-second");
}

#[test]
fn rotate_is_fair_and_advances_by_original_position_when_members_disappear() {
    let mut router = router(Strategy::Rotate, &["z", "a", "m"]);
    let mut snapshots = [ready("m"), ready("z"), ready("a")];
    let sequence: Vec<_> = (0..300)
        .map(|_| account(&mut router, &snapshots, context()).expect("all members ready"))
        .collect();
    for id in ["z", "a", "m"] {
        assert_eq!(
            sequence.iter().filter(|value| value.as_str() == id).count(),
            100
        );
    }
    assert_eq!(&sequence[..6], ["z", "a", "m", "z", "a", "m"]);
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("z")
    );
    snapshots[2].service = ServiceState::Paused;
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("m")
    );
    snapshots[2].service = ServiceState::Ready;
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("z")
    );
    router.reset();
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("z")
    );
}

fn weighted_router() -> Router {
    let mut configured = group("main", Strategy::Weight, &["a", "b"]);
    configured.members[0].weight = Some(3);
    configured.members[1].weight = Some(1);
    Router::new(vec![configured], Limits::default()).expect("3:1 weights are valid")
}

#[test]
fn smooth_three_to_one_has_fair_prefixes_and_never_four_same_in_a_row() {
    let mut router = weighted_router();
    let snapshots = [ready("a"), ready("b")];
    let mut count_a = 0_i64;
    let mut run = 0;
    let mut previous = String::new();
    let mut sequence = Vec::new();
    for n in 1_i64..=4_000 {
        let selected = account(&mut router, &snapshots, context()).expect("weighted member");
        if selected == "a" {
            count_a += 1;
        }
        assert!(
            (4 * count_a - 3 * n).abs() <= 2,
            "3:1 prefix distribution must stay smooth at request {n}"
        );
        run = if selected == previous { run + 1 } else { 1 };
        assert!(run < 4, "no four consecutive selections at request {n}");
        previous.clone_from(&selected);
        sequence.push(selected);
    }
    assert_eq!(count_a, 3_000);
    assert_eq!(&sequence[..8], ["a", "a", "b", "a", "a", "a", "b", "a"]);
}

#[test]
fn implicit_weights_are_one_and_maximum_weight_is_supported() {
    let mut equal = router(Strategy::Weight, &["z", "a", "m"]);
    let snapshots = [ready("a"), ready("m"), ready("z")];
    let actual: Vec<_> = (0..6)
        .map(|_| account(&mut equal, &snapshots, context()).expect("equal-weight member"))
        .collect();
    assert_eq!(actual, ["z", "a", "m", "z", "a", "m"]);
    let mut configured = group("main", Strategy::Weight, &["a", "b"]);
    configured.members[0].weight = Some(1000);
    let mut maximum = Router::new(vec![configured], Limits::default()).expect("maximum weight");
    let snapshots = [ready("a"), ready("b")];
    let count = (0..1001)
        .filter(|_| account(&mut maximum, &snapshots, context()).as_deref() == Some("a"))
        .count();
    assert_eq!(count, 1000);
}

#[test]
fn excluded_weight_members_accrue_no_catchup_credit_and_empty_pools_consume_no_turn() {
    let mut router = weighted_router();
    let mut snapshots = [ready("a"), ready("b")];
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("a")
    );
    snapshots[0].service = ServiceState::Paused;
    for _ in 0..100 {
        assert_eq!(
            account(&mut router, &snapshots, context()).as_deref(),
            Some("b")
        );
    }
    snapshots[1].service = ServiceState::Paused;
    let empty = router
        .select("group/main", &snapshots, context())
        .expect("empty eligible pool is a decision");
    assert!(empty.selected.is_none());
    assert_eq!(empty.candidates.len(), 2);
    snapshots[0].service = ServiceState::Ready;
    snapshots[1].service = ServiceState::Ready;
    let actual: Vec<_> = (0..4)
        .map(|_| account(&mut router, &snapshots, context()).expect("restored pool"))
        .collect();
    assert_eq!(actual, ["a", "a", "b", "a"]);
}

#[test]
fn smart_prefers_fresh_subscription_reset_without_inventing_api_quota() {
    let mut router = router(Strategy::Smart, &["api", "later", "sooner", "unknown"]);
    let mut snapshots = [
        ready("api"),
        subscription("later", 999, NOW + 500),
        subscription("sooner", 1, NOW + 10),
        subscription("unknown", 50, NOW + 5),
    ];
    snapshots[3].quota = Quota::Unknown;
    let decision = router
        .select("group/main", &snapshots, context())
        .expect("smart selection");
    let selected = decision.selected.expect("fresh subscription is available");
    assert_eq!(selected.target.account, "sooner");
    assert_eq!(
        selected.reason,
        SelectionReason::SubscriptionReset(NOW + 10)
    );
    assert_eq!(rejection(&decision.candidates[3]), Exclusion::UnknownQuota);
    snapshots[2].quota = Quota::Observed(observed(QuotaWindow {
        remaining: 1,
        unit: Unit::Requests,
        resets_at: NOW,
        period: QuotaPeriod::Other,
    }));
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("later")
    );
    snapshots[1].quota = Quota::Unknown;
    let decision = router
        .select("group/main", &snapshots, context())
        .expect("known API readiness is still usable");
    assert_eq!(decision.selected.expect("API member").target.account, "api");
    assert_eq!(decision.fallback, Some(Fallback::NoSubscriptionQuota));
    assert_eq!(snapshots[0].quota, Quota::NotProvided);
}

fn usage_window() -> UsageWindow {
    UsageWindow {
        starts_at: NOW - 100,
        ends_at: NOW + 100,
        unit: Unit::Tokens,
    }
}

#[test]
fn usage_compares_real_zero_in_one_explicit_window() {
    let mut router = router(Strategy::Usage, &["first", "zero", "third"]);
    let snapshots = [("first", 20), ("zero", 0), ("third", 10)].map(|(id, used)| Snapshot {
        usage: Some(observed(Usage {
            window: usage_window(),
            used,
        })),
        ..ready(id)
    });
    let context = Context {
        usage_window: Some(usage_window()),
        ..context()
    };
    let decision = router
        .select("group/main", &snapshots, context)
        .expect("usage");
    let selected = decision.selected.expect("zero usage is measured");
    assert_eq!(selected.target.account, "zero");
    assert_eq!(
        selected.reason,
        SelectionReason::Usage(Usage {
            window: usage_window(),
            used: 0,
        })
    );
}

#[test]
fn usage_unknown_stale_future_or_incomparable_data_falls_back_without_zero_fill() {
    for variation in 0..6 {
        let mut router = router(Strategy::Usage, &["first", "second"]);
        let mut snapshots = ["first", "second"].map(|id| Snapshot {
            usage: Some(observed(Usage {
                window: usage_window(),
                used: if id == "first" { 90 } else { 1 },
            })),
            ..ready(id)
        });
        let mut context = Context {
            usage_window: Some(usage_window()),
            ..context()
        };
        match variation {
            0 => context.usage_window = None,
            1 => snapshots[1].usage = None,
            2 => snapshots[1].usage.as_mut().expect("sample").valid_until = NOW,
            3 => snapshots[1].usage.as_mut().expect("sample").observed_at = NOW + 1,
            4 => {
                snapshots[1]
                    .usage
                    .as_mut()
                    .expect("sample")
                    .value
                    .window
                    .unit = Unit::Requests
            }
            5 => {
                snapshots[1]
                    .usage
                    .as_mut()
                    .expect("sample")
                    .value
                    .window
                    .starts_at -= 1
            }
            _ => unreachable!(),
        }
        let decision = router
            .select("group/main", &snapshots, context)
            .expect("usage fallback");
        assert_eq!(
            decision.selected.expect("fallback member").target.account,
            "first",
            "variation {variation}"
        );
        assert_eq!(
            decision.fallback,
            Some(if variation == 0 {
                Fallback::MissingUsageWindow
            } else {
                Fallback::IncompleteUsage
            })
        );
    }
}

#[test]
fn pace_uses_weekly_quota_per_remaining_time_and_earliest_reset_on_equal_ratios() {
    let mut router = router(
        Strategy::Pace,
        &["low-rate", "higher-rate", "equal-earlier"],
    );
    let snapshots = [
        subscription("low-rate", 100, NOW + 100),
        subscription("higher-rate", 20, NOW + 10),
        subscription("equal-earlier", 10, NOW + 5),
    ];
    let decision = router
        .select("group/main", &snapshots, context())
        .expect("pace");
    let selected = decision.selected.expect("weekly quota permits selection");
    assert_eq!(selected.target.account, "equal-earlier");
    assert_eq!(
        selected.reason,
        SelectionReason::WeeklyPace {
            remaining: 10,
            seconds_to_reset: 5,
            unit: Unit::Tokens,
        }
    );
}

#[test]
fn pace_does_not_treat_missing_weekly_quota_or_different_units_as_comparable() {
    for variation in 0..3 {
        let mut router = router(Strategy::Pace, &["first", "second"]);
        let mut snapshots = [
            subscription("first", 1, NOW + 1000),
            subscription("second", 100, NOW + 1),
        ];
        match variation {
            0 => snapshots[1] = ready("second"),
            1 => {
                if let Quota::Observed(o) = &mut snapshots[1].quota {
                    o.value.period = QuotaPeriod::Other;
                }
            }
            2 => {
                if let Quota::Observed(o) = &mut snapshots[1].quota {
                    o.value.unit = Unit::Requests;
                }
            }
            _ => unreachable!(),
        }
        let decision = router
            .select("group/main", &snapshots, context())
            .expect("pace fallback");
        assert_eq!(decision.selected.expect("fallback").target.account, "first");
        assert_eq!(decision.fallback, Some(Fallback::IncomparableWeeklyQuota));
    }
}

#[test]
fn large_pace_values_are_compared_exactly_without_overflow_or_float_rounding() {
    let mut router = router(Strategy::Pace, &["first", "second"]);
    let snapshots = [
        subscription("first", u64::MAX - 1, u64::MAX),
        subscription("second", u64::MAX, u64::MAX),
    ];
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("second")
    );
}

#[test]
fn fast_uses_real_latency_and_throughput_with_explicit_output_assumptions() {
    let mut router = router(Strategy::Fast, &["quick-first", "quick-generation"]);
    let snapshots = [
        Snapshot {
            speed: Some(observed(Speed {
                first_token_ms: 10,
                milli_tokens_per_second: 1_000,
            })),
            ..ready("quick-first")
        },
        Snapshot {
            speed: Some(observed(Speed {
                first_token_ms: 100,
                milli_tokens_per_second: 100_000,
            })),
            ..ready("quick-generation")
        },
    ];
    let decision = router
        .select("group/main", &snapshots, context())
        .expect("latency");
    let selected = decision.selected.expect("speed measured");
    assert_eq!(selected.target.account, "quick-first");
    assert!(matches!(
        selected.reason,
        SelectionReason::LatencyThenThroughput(_)
    ));
    let decision = router
        .select(
            "group/main",
            &snapshots,
            Context {
                expected_output_tokens: Some(100),
                ..context()
            },
        )
        .expect("estimated completion");
    let selected = decision.selected.expect("speed measured");
    assert_eq!(selected.target.account, "quick-generation");
    assert!(matches!(
        selected.reason,
        SelectionReason::EstimatedCompletion {
            output_tokens: 100,
            ..
        }
    ));
}

#[test]
fn fast_unknown_stale_future_and_zero_speed_never_become_artificially_fast() {
    for variation in 0..4 {
        let mut router = router(Strategy::Fast, &["first", "second"]);
        let mut snapshots = ["first", "second"].map(|id| Snapshot {
            speed: Some(observed(Speed {
                first_token_ms: if id == "first" { 100 } else { 1 },
                milli_tokens_per_second: 10_000,
            })),
            ..ready(id)
        });
        match variation {
            0 => snapshots[1].speed = None,
            1 => snapshots[1].speed.as_mut().expect("sample").valid_until = NOW,
            2 => snapshots[1].speed.as_mut().expect("sample").observed_at = NOW + 1,
            3 => {
                snapshots[1]
                    .speed
                    .as_mut()
                    .expect("sample")
                    .value
                    .milli_tokens_per_second = 0;
            }
            _ => unreachable!(),
        }
        let decision = router
            .select("group/main", &snapshots, context())
            .expect("speed fallback");
        assert_eq!(decision.selected.expect("fallback").target.account, "first");
        assert_eq!(decision.fallback, Some(Fallback::IncompleteSpeed));
    }
}

#[test]
fn fast_ties_use_throughput_then_user_order_and_large_inputs_are_bounded() {
    let mut router = router(Strategy::Fast, &["z", "a"]);
    let mut snapshots = ["z", "a"].map(|id| Snapshot {
        speed: Some(observed(Speed {
            first_token_ms: u32::MAX,
            milli_tokens_per_second: u32::MAX,
        })),
        ..ready(id)
    });
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("z")
    );
    snapshots[0]
        .speed
        .as_mut()
        .expect("sample")
        .value
        .milli_tokens_per_second -= 1;
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("a")
    );
    assert_eq!(
        account(
            &mut router,
            &snapshots,
            Context {
                expected_output_tokens: Some(u32::MAX),
                ..context()
            }
        )
        .as_deref(),
        Some("a")
    );
}

#[test]
fn policy_names_and_speed_aliases_are_explicit() {
    for (name, expected) in [
        ("smart", Strategy::Smart),
        ("order", Strategy::Order),
        ("rotate", Strategy::Rotate),
        ("usage", Strategy::Usage),
        ("pace", Strategy::Pace),
        ("weight", Strategy::Weight),
        ("fast", Strategy::Fast),
        ("fastest", Strategy::Fast),
        ("speed", Strategy::Fast),
    ] {
        assert_eq!(
            name.parse::<Strategy>().expect("documented policy"),
            expected
        );
    }
    assert_eq!(
        "unknown".parse::<Strategy>(),
        Err(RoutingError::UnknownStrategy)
    );
    assert_eq!(Strategy::default(), Strategy::Smart);
}

#[test]
fn a_provider_catalog_entry_without_service_observations_serves_nothing() {
    let mut router = router(Strategy::Smart, &["configured-only"]);
    let decision = router
        .select("group/main", &[], context())
        .expect("no observations");
    assert!(decision.selected.is_none());
    assert_eq!(
        rejection(&decision.candidates[0]),
        Exclusion::MissingSnapshot
    );
    assert!(decision.fallback.is_none());
}

#[test]
fn nonservice_budget_permission_and_quota_states_are_distinct_explanations() {
    let cases = [
        (
            ServiceState::Unknown,
            Exclusion::Service(ServiceState::Unknown),
        ),
        (
            ServiceState::Paused,
            Exclusion::Service(ServiceState::Paused),
        ),
        (
            ServiceState::Disabled,
            Exclusion::Service(ServiceState::Disabled),
        ),
        (
            ServiceState::NeedsLogin,
            Exclusion::Service(ServiceState::NeedsLogin),
        ),
        (
            ServiceState::UnsupportedModel,
            Exclusion::Service(ServiceState::UnsupportedModel),
        ),
        (
            ServiceState::Unavailable,
            Exclusion::Service(ServiceState::Unavailable),
        ),
    ];
    for (state, expected) in cases {
        let mut router = router(Strategy::Order, &["blocked", "ready"]);
        let mut blocked = ready("blocked");
        blocked.service = state;
        let decision = router
            .select("group/main", &[blocked, ready("ready")], context())
            .expect("explicit service state");
        assert_eq!(
            decision.selected.expect("alternate").target.account,
            "ready"
        );
        assert_eq!(rejection(&decision.candidates[0]), expected);
    }
    for (access, budget, quota, expected) in [
        (
            ModelAccess::Unknown,
            Budget::Unrestricted,
            Quota::NotProvided,
            Exclusion::ModelAccess(ModelAccess::Unknown),
        ),
        (
            ModelAccess::Denied,
            Budget::Unrestricted,
            Quota::NotProvided,
            Exclusion::ModelAccess(ModelAccess::Denied),
        ),
        (
            ModelAccess::Allowed,
            Budget::Unknown,
            Quota::NotProvided,
            Exclusion::Budget(Budget::Unknown),
        ),
        (
            ModelAccess::Allowed,
            Budget::Exceeded,
            Quota::NotProvided,
            Exclusion::Budget(Budget::Exceeded),
        ),
        (
            ModelAccess::Allowed,
            Budget::WithinLimit,
            Quota::Unknown,
            Exclusion::UnknownQuota,
        ),
    ] {
        let mut router = router(Strategy::Order, &["blocked"]);
        let mut snapshot = ready("blocked");
        snapshot.access = access;
        snapshot.budget = budget;
        snapshot.quota = quota;
        let decision = router
            .select("group/main", &[snapshot], context())
            .expect("filter");
        assert!(decision.selected.is_none());
        assert_eq!(rejection(&decision.candidates[0]), expected);
    }
}

#[test]
fn quota_unknown_zero_stale_future_invalid_and_reset_are_never_replenished() {
    for variation in 0..7 {
        let mut router = router(Strategy::Order, &["subscription"]);
        let mut snapshot = subscription("subscription", 10, NOW + 100);
        let expected = match variation {
            0 => {
                snapshot.quota = Quota::Unknown;
                Exclusion::UnknownQuota
            }
            1 => {
                snapshot.quota = Quota::NotProvided;
                Exclusion::UnknownQuota
            }
            _ => {
                let Quota::Observed(o) = &mut snapshot.quota else {
                    unreachable!()
                };
                match variation {
                    2 => {
                        o.value.remaining = 0;
                        Exclusion::ExhaustedQuota
                    }
                    3 => {
                        o.valid_until = NOW;
                        Exclusion::StaleQuota
                    }
                    4 => {
                        o.observed_at = NOW + 1;
                        Exclusion::StaleQuota
                    }
                    5 => {
                        o.value.resets_at = NOW;
                        Exclusion::StaleQuota
                    }
                    6 => {
                        o.value.unit = Unit::BasisPoints;
                        o.value.remaining = 10_001;
                        Exclusion::InvalidQuota
                    }
                    _ => unreachable!(),
                }
            }
        };
        let decision = router
            .select("group/main", &[snapshot], context())
            .expect("quota");
        assert!(decision.selected.is_none(), "variation {variation}");
        assert_eq!(rejection(&decision.candidates[0]), expected);
    }
}

#[test]
fn expired_and_future_service_snapshots_do_not_become_ready() {
    for future in [false, true] {
        let mut router = router(Strategy::Order, &["a"]);
        let mut snapshot = ready("a");
        if future {
            snapshot.observed_at = NOW + 1;
        } else {
            snapshot.valid_until = NOW;
        }
        let decision = router
            .select("group/main", &[snapshot], context())
            .expect("freshness");
        assert!(decision.selected.is_none());
        assert_eq!(rejection(&decision.candidates[0]), Exclusion::StaleSnapshot);
    }
}

#[test]
fn permitted_api_and_keyless_local_accounts_need_no_fabricated_subscription_quota() {
    for kind in [AccountKind::ApiKey, AccountKind::Local] {
        let mut router = router(Strategy::Order, &["a"]);
        let snapshot = Snapshot {
            kind,
            budget: Budget::WithinLimit,
            ..ready("a")
        };
        assert_eq!(
            account(&mut router, &[snapshot], context()).as_deref(),
            Some("a")
        );
    }
}

#[test]
fn plan_limited_models_only_win_after_all_unrestricted_candidates_are_unserviceable() {
    let mut router = router(Strategy::Order, &["limited", "included"]);
    let mut snapshots = [ready("limited"), ready("included")];
    snapshots[0].access = ModelAccess::PlanLimited;
    let decision = router
        .select("group/main", &snapshots, context())
        .expect("plan");
    assert_eq!(
        decision.selected.expect("included model").target.account,
        "included"
    );
    snapshots[1].budget = Budget::Exceeded;
    let decision = router
        .select("group/main", &snapshots, context())
        .expect("last option");
    assert!(
        decision
            .selected
            .expect("limited model is last resort")
            .priority
            .plan_limited
    );
}

#[test]
fn soft_rate_sink_and_resting_are_explicit_last_choices_after_normal_members() {
    let mut router = router(Strategy::Order, &["resting", "rate", "normal"]);
    let mut snapshots = [
        subscription("resting", 10, NOW + 100),
        subscription("rate", 10, NOW + 100),
        ready("normal"),
    ];
    snapshots[0].resting_until = Some(NOW + 10);
    snapshots[1].rate_limited_until = Some(NOW + 10);
    let context = Context {
        rate_limit_policy: RateLimitPolicy::Sink,
        resting_policy: RestingPolicy::LastResort,
        ..context()
    };
    assert_eq!(
        account(&mut router, &snapshots, context).as_deref(),
        Some("normal")
    );
    snapshots[2].service = ServiceState::Unavailable;
    assert_eq!(
        account(&mut router, &snapshots, context).as_deref(),
        Some("rate")
    );
    snapshots[1].budget = Budget::Exceeded;
    let decision = router
        .select("group/main", &snapshots, context)
        .expect("resting choice");
    assert_eq!(
        decision
            .selected
            .expect("resting permitted")
            .priority
            .recovery_stage,
        2
    );
    assert_eq!(
        account(&mut router, &snapshots, context).as_deref(),
        Some("resting")
    );
}

#[test]
fn defaults_skip_soft_limits_and_hard_retry_deadlines_override_last_resort_policies() {
    let mut router = router(Strategy::Order, &["rate", "resting", "hard"]);
    let mut snapshots = [
        subscription("rate", 10, NOW + 100),
        subscription("resting", 10, NOW + 100),
        subscription("hard", 10, NOW + 100),
    ];
    snapshots[0].rate_limited_until = Some(NOW + 10);
    snapshots[1].resting_until = Some(NOW + 10);
    snapshots[2].retry_not_before = Some(NOW + 10);
    let decision = router
        .select("group/main", &snapshots, context())
        .expect("skip");
    assert!(decision.selected.is_none());
    assert_eq!(
        rejection(&decision.candidates[0]),
        Exclusion::RateLimited(NOW + 10)
    );
    assert_eq!(
        rejection(&decision.candidates[1]),
        Exclusion::Resting(NOW + 10)
    );
    assert_eq!(
        rejection(&decision.candidates[2]),
        Exclusion::RetryNotBefore(NOW + 10)
    );
    for snapshot in &mut snapshots {
        snapshot.retry_not_before = Some(NOW + 10);
    }
    assert!(
        account(
            &mut router,
            &snapshots,
            Context {
                rate_limit_policy: RateLimitPolicy::Sink,
                resting_policy: RestingPolicy::LastResort,
                ..context()
            }
        )
        .is_none()
    );
    assert_eq!(
        account(
            &mut router,
            &snapshots,
            Context {
                now: NOW + 10,
                ..context()
            }
        )
        .as_deref(),
        Some("rate")
    );
}

#[test]
fn soft_sinking_without_known_quota_does_not_assume_an_api_key_has_allowance() {
    let mut router = router(Strategy::Order, &["a"]);
    let snapshot = Snapshot {
        rate_limited_until: Some(NOW + 10),
        ..ready("a")
    };
    let decision = router
        .select(
            "group/main",
            &[snapshot],
            Context {
                rate_limit_policy: RateLimitPolicy::Sink,
                ..context()
            },
        )
        .expect("known quota required for sinking");
    assert!(decision.selected.is_none());
    assert_eq!(
        rejection(&decision.candidates[0]),
        Exclusion::RateLimited(NOW + 10)
    );
}

#[test]
fn nested_expansion_preserves_order_and_root_strategy_is_explicit() {
    let mut root = group("main", Strategy::Order, &["z"]);
    root.members.push(Member::group("child"));
    root.members.push(Member::model(target("last")));
    let child = group("child", Strategy::Rotate, &["b", "a"]);
    let mut router = Router::new(vec![child, root], Limits::default()).expect("nested group");
    assert_eq!(router.groups()[0].id, "child");
    let members = router
        .expanded_members("group/main")
        .expect("root expansion");
    let order: Vec<_> = members.iter().map(|m| m.target.account.as_str()).collect();
    assert_eq!(order, ["z", "b", "a", "last"]);
    assert_eq!(members[0].target.model_id(), "fixture/coding");
    let snapshots = [ready("last"), ready("a"), ready("z"), ready("b")];
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("z")
    );
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("z")
    );
    for expected in ["b", "a", "b"] {
        let decision = router
            .select("group/child", &snapshots, context())
            .expect("child policy");
        assert_eq!(
            decision.selected.expect("child member").target.account,
            expected
        );
    }
}

#[test]
fn disabled_ancestors_and_members_remain_visible_and_do_not_serve() {
    let mut root = group("main", Strategy::Order, &["first"]);
    root.members[0].enabled = false;
    root.members.push(Member::group("child"));
    let mut child = group("child", Strategy::Order, &["nested"]);
    child.enabled = false;
    let mut router =
        Router::new(vec![root, child], Limits::default()).expect("disabled groups retained");
    let decision = router
        .select("group/main", &[ready("first"), ready("nested")], context())
        .expect("disabled decision");
    assert!(decision.selected.is_none());
    assert!(
        decision
            .candidates
            .iter()
            .all(|c| rejection(c) == Exclusion::Disabled)
    );
    assert!(
        router
            .select("group/child", &[ready("nested")], context())
            .expect("disabled child")
            .selected
            .is_none()
    );
}

#[test]
fn empty_missing_and_cyclic_groups_are_rejected_even_if_disabled() {
    assert_eq!(
        Router::new(
            vec![group("empty", Strategy::Order, &[])],
            Limits::default()
        )
        .expect_err("empty group"),
        RoutingError::EmptyGroup("empty".to_owned())
    );
    let mut missing = group("main", Strategy::Order, &["a"]);
    missing.members.push(Member::group("missing"));
    assert_eq!(
        Router::new(vec![missing], Limits::default()).expect_err("missing nested group"),
        RoutingError::UnknownGroup("missing".to_owned())
    );
    for cycle_length in [1, 2, 3] {
        let groups: Vec<_> = (0..cycle_length)
            .map(|i| {
                let mut value = group(&format!("g{i}"), Strategy::Order, &["unused"]);
                value.enabled = false;
                value.members = vec![Member::group(&format!("g{}", (i + 1) % cycle_length))];
                value
            })
            .collect();
        let error = Router::new(groups, Limits::default()).expect_err("cycle is unsafe");
        let RoutingError::Cycle(path) = &error else {
            panic!("expected cycle, got {error}")
        };
        assert_eq!(path.len(), cycle_length + 1);
        assert_eq!(path.first(), path.last());
        assert!(error.to_string().contains("cycle"));
    }
}

#[test]
fn depth_limit_counts_the_root_and_accepts_the_exact_boundary() {
    let groups = || {
        (0..3)
            .map(|i| {
                let mut value = group(&format!("g{i}"), Strategy::Order, &["a"]);
                if i < 2 {
                    value.members = vec![Member::group(&format!("g{}", i + 1))];
                }
                value
            })
            .collect()
    };
    assert!(
        Router::new(
            groups(),
            Limits {
                depth: 3,
                ..Limits::default()
            }
        )
        .is_ok()
    );
    assert_eq!(
        Router::new(
            groups(),
            Limits {
                depth: 2,
                ..Limits::default()
            }
        )
        .expect_err("depth three exceeds two"),
        RoutingError::DepthLimit("g2".to_owned())
    );
}

#[test]
fn shared_dag_references_are_not_reported_as_cycles_but_duplicate_targets_are_rejected() {
    let mut root = group("main", Strategy::Order, &["z"]);
    root.members = vec![Member::group("left"), Member::group("right")];
    let mut left = group("left", Strategy::Order, &["l"]);
    left.members.push(Member::group("shared"));
    let mut right = group("right", Strategy::Order, &["r"]);
    right.members.push(Member::group("shared"));
    let shared = group("shared", Strategy::Order, &["s"]);
    assert_eq!(
        Router::new(vec![root, left, right, shared], Limits::default())
            .expect_err("ambiguous duplicate weight"),
        RoutingError::DuplicateTarget("main".to_owned())
    );
}

#[test]
fn invalid_leaf_and_hierarchical_weights_are_not_silently_reinterpreted() {
    for weight in [0, 1001, u16::MAX] {
        let mut root = group("main", Strategy::Weight, &["a"]);
        root.members[0].weight = Some(weight);
        assert_eq!(
            Router::new(vec![root], Limits::default()).expect_err("invalid weight"),
            RoutingError::InvalidWeight("main".to_owned())
        );
    }
    let mut root = group("main", Strategy::Weight, &["a"]);
    root.members = vec![Member {
        weight: Some(3),
        ..Member::group("child")
    }];
    assert_eq!(
        Router::new(
            vec![root, group("child", Strategy::Weight, &["b"])],
            Limits::default()
        )
        .expect_err("group weight requires composition"),
        RoutingError::NestedWeight("main".to_owned())
    );
}

#[test]
fn decision_groups_require_explicit_purpose_and_cannot_leak_into_chat_expansion() {
    let mut decision = group("decision", Strategy::Order, &["a"]);
    decision.purpose = Purpose::Decision;
    let mut router =
        Router::new(vec![decision.clone()], Limits::default()).expect("decision group");
    assert_eq!(
        router
            .select("group/decision", &[ready("a")], context())
            .expect_err("chat request"),
        RoutingError::PurposeMismatch("decision".to_owned())
    );
    assert!(
        router
            .select(
                "group/decision",
                &[ready("a")],
                Context {
                    purpose: Purpose::Decision,
                    ..context()
                }
            )
            .expect("explicit decision endpoint")
            .selected
            .is_some()
    );
    let mut chat = group("chat", Strategy::Order, &["b"]);
    chat.members.push(Member::group("decision"));
    assert_eq!(
        Router::new(vec![chat, decision], Limits::default()).expect_err("chat/decision mix"),
        RoutingError::PurposeMismatch("decision".to_owned())
    );
}

#[test]
fn all_configuration_work_and_snapshot_bounds_have_observable_errors() {
    let configured = || vec![group("main", Strategy::Order, &["a", "b"])];
    let exact = Limits {
        groups: 1,
        members: 2,
        expanded_members: 2,
        depth: 1,
        expansion_steps: 3,
        snapshots: 2,
    };
    let mut router = Router::new(configured(), exact).expect("all exact construction boundaries");
    assert_eq!(
        account(&mut router, &[ready("a"), ready("b")], context()).as_deref(),
        Some("a")
    );
    for (limits, resource) in [
        (
            Limits {
                members: 1,
                ..exact
            },
            "declared members",
        ),
        (
            Limits {
                expanded_members: 1,
                ..exact
            },
            "expanded members",
        ),
        (
            Limits {
                expansion_steps: 2,
                ..exact
            },
            "expansion steps",
        ),
    ] {
        assert_eq!(
            Router::new(configured(), limits).expect_err("bound exceeded"),
            RoutingError::LimitExceeded(resource)
        );
    }
    assert_eq!(
        Router::new(
            vec![
                group("a", Strategy::Order, &["a"]),
                group("b", Strategy::Order, &["b"])
            ],
            exact
        )
        .expect_err("group bound"),
        RoutingError::LimitExceeded("groups")
    );
    assert_eq!(
        router
            .select(
                "group/main",
                &[ready("a"), ready("b"), ready("unrelated")],
                context()
            )
            .expect_err("snapshot bound"),
        RoutingError::LimitExceeded("snapshots")
    );
    for limits in [
        Limits {
            depth: 0,
            ..Limits::default()
        },
        Limits {
            depth: usize::MAX,
            ..Limits::default()
        },
        Limits {
            groups: 129,
            ..Limits::default()
        },
    ] {
        assert_eq!(
            Router::new(Vec::new(), limits).expect_err("invalid safety limit"),
            RoutingError::InvalidLimits
        );
    }
}

#[test]
fn expansion_limit_is_global_including_directly_requested_children() {
    let mut root = group("main", Strategy::Order, &["a"]);
    root.members = vec![Member::group("child")];
    assert_eq!(
        Router::new(
            vec![root, group("child", Strategy::Order, &["b"])],
            Limits {
                expanded_members: 1,
                ..Limits::default()
            }
        )
        .expect_err("one root leaf plus one direct child leaf"),
        RoutingError::LimitExceeded("expanded members")
    );
}

#[test]
fn invalid_requests_do_not_advance_weighted_scheduling_and_clock_guard_is_explicit() {
    let mut router = weighted_router();
    let snapshots = [ready("a"), ready("b")];
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("a")
    );
    assert_eq!(
        router
            .select("group/missing", &snapshots, context())
            .expect_err("unknown group"),
        RoutingError::UnknownGroup("missing".to_owned())
    );
    assert_eq!(
        router
            .select("main", &snapshots, context())
            .expect_err("explicit namespace"),
        RoutingError::InvalidIdentifier("group reference")
    );
    assert_eq!(
        router
            .select("group/main", &[ready("a"), ready("a")], context())
            .expect_err("duplicate snapshot"),
        RoutingError::DuplicateSnapshot
    );
    assert_eq!(
        router
            .select(
                "group/main",
                &snapshots,
                Context {
                    now: NOW - 1,
                    ..context()
                }
            )
            .expect_err("regressing time"),
        RoutingError::ClockMovedBackwards
    );
    assert_eq!(
        router
            .select(
                "group/main",
                &snapshots,
                Context {
                    usage_window: Some(UsageWindow {
                        starts_at: NOW,
                        ends_at: NOW,
                        unit: Unit::Tokens
                    }),
                    ..context()
                }
            )
            .expect_err("invalid interval"),
        RoutingError::InvalidUsageWindow
    );
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("a")
    );
    assert_eq!(
        account(&mut router, &snapshots, context()).as_deref(),
        Some("b")
    );
    router.reset();
    assert_eq!(
        account(
            &mut router,
            &snapshots,
            Context {
                now: NOW - 1,
                ..context()
            }
        )
        .as_deref(),
        Some("a")
    );
}

#[test]
fn unconfigured_snapshot_ids_do_not_create_persistent_routing_members() {
    let mut router = router(Strategy::Rotate, &["a", "b"]);
    for i in 0..1000 {
        let snapshots = [ready("a"), ready("b"), ready(&format!("unrelated-{i}"))];
        assert_eq!(
            account(&mut router, &snapshots, context()).as_deref(),
            Some(if i % 2 == 0 { "a" } else { "b" })
        );
    }
    let members = router
        .expanded_members("group/main")
        .expect("unchanged expansion");
    assert_eq!(members.len(), 2);
    assert_eq!(router.groups().len(), 1);
}

#[test]
fn duplicate_groups_invalid_ids_and_ambiguous_model_paths_are_rejected() {
    assert_eq!(
        Router::new(
            vec![
                group("same", Strategy::Order, &["a"]),
                group("same", Strategy::Order, &["b"])
            ],
            Limits::default()
        )
        .expect_err("duplicate groups"),
        RoutingError::DuplicateGroup("same".to_owned())
    );
    for variation in 0..5 {
        let mut root = group("main", Strategy::Order, &["a"]);
        let MemberTarget::Model(value) = &mut root.members[0].target else {
            unreachable!()
        };
        match variation {
            0 => value.provider = "group".to_owned(),
            1 => value.model = "a/../b".to_owned(),
            2 => value.account = String::new(),
            3 => value.account = "private-fixture?secret".to_owned(),
            4 => value.model = "a".repeat(257),
            _ => unreachable!(),
        }
        let error = Router::new(vec![root], Limits::default()).expect_err("invalid identity");
        assert!(matches!(error, RoutingError::InvalidIdentifier(_)));
        assert!(!format!("{error} {error:?}").contains("private-fixture"));
    }
    let mut valid = group("main", Strategy::Order, &["a"]);
    if let MemberTarget::Model(value) = &mut valid.members[0].target {
        value.model = "namespace/model:latest@v1".to_owned();
    }
    assert!(Router::new(vec![valid], Limits::default()).is_ok());
}
