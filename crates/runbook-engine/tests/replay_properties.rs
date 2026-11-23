//! Determinism and replay as properties over random command sequences.
//!
//! A seeded generator drives a set of runbooks with mostly valid actor
//! actions (reports, claims, votes, signals, checklist ticks, overrides,
//! pauses, cancellation, clock ticks) mixed with deliberately invalid ones.
//! For every run it checks:
//! - every command `decide`/`advance_time` returns is accepted by `apply`,
//!   and `decide` never fails on a reachable state;
//! - folding the stored (JSON) form of the emitted events from a fresh
//!   restored definition reproduces the live state, at random prefixes and
//!   at the end;
//! - running the same actions again yields byte-identical events;
//! - a rejected action emits nothing: rerunning only the accepted actions
//!   yields the same event bytes;
//! - a closed execution holds no open step and accepts no further actor
//!   input except overrides.
mod support;
use runbook_engine::*;
use runbook_model::{
    ActorRef, Data, ExecutionStatus, RecipientRef, ReportStatus, StepRecord, StepStatus, Verdict,
};
use runbook_spec::ir::{NodeKind, WaitKind};
use serde_json::json;
use support::*;

/// splitmix64: tiny, deterministic and target independent.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const HEADER: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: prop, title: Prop, owners: [team:leads]}\n";
const RUNBOOKS: &[(&str, &str)] = &[
    (
        "retry_handoff_catch_finally",
        "spec:
  execution: {timeout: 2h, cleanup_timeout: 20m}
  defaults: {retry: {max_attempts: 2, backoff: 1m}}
  steps:
    - id: grp
      steps:
        - {id: a, title: A, check: true, timeout: 10m}
        - {id: b, title: B, on_failure: handoff, checklist: [one, two]}
      catch: [{id: fix, title: Fix}]
      finally: [{id: tidy, title: Tidy}]
    - {id: c, title: C, on_failure: continue, skippable: true}
  finally: [{id: notify, title: Notify, timeout: 15m}]
",
    ),
    (
        "parallel_foreach",
        "spec:
  execution: {timeout: 1h, cleanup_timeout: 10m}
  inputs: {items: {type: list, items: {type: string}, default: [east, west, north]}}
  steps:
    - id: fan
      fail_fast: true
      parallel:
        - {id: p1, title: P1, check: true, on_failure: continue}
        - id: p2
          steps: [{id: p2a, title: P2A}]
          finally: [{id: p2clean, title: Clean}]
        - {id: p3, wait: {signal: go, timeout: 10m}}
    - id: each
      foreach: {items: '${{ inputs.items }}', key: '${{ item }}', max_items: 3, concurrency: 2, steps: [{id: e, title: E, check: true, on_failure: continue}]}
",
    ),
    (
        "loop_switch_set",
        "spec:
  vars: {result: none}
  steps:
    - id: probe
      loop:
        max_iterations: 3
        delay: 2m
        until: '${{ steps.check.outputs.rate <= 0.01 }}'
        steps: [{id: check, title: Check, check: true, outputs: {rate: {type: number, required: true}}}]
    - id: route
      switch:
        cases:
          - when: \"${{ steps.probe.verdict == 'fail' }}\"
            steps: [{id: bad, set: {vars: {result: bad}}}]
        default: {steps: [{id: good, set: {vars: {result: good}}}]}
  outputs: {result: '${{ vars.result }}'}
",
    ),
    (
        "approvals",
        "spec:
  execution: {timeout: 3h, cleanup_timeout: 10m}
  steps:
    - id: gate
      approval: {prompt: Go?, options: [go, stop], approvers: {teams: [oncall]}, min: 2, timeout: 30m, escalation_timeout: 30m}
    - id: deploy
      if: \"${{ steps.gate.outputs.decision == 'go' }}\"
      title: Deploy
      destructive: true
    - id: auto
      approval:
        prompt: Auto?
        options: [{id: proceed, title: Proceed, auto: true}, {id: halt, title: Halt}]
        approvers: {teams: [oncall]}
        timeout: 20m
        on_timeout: {decide: proceed}
    - {id: after, title: After}
  finally: [{id: notify, title: Notify}]
",
    ),
    (
        "waits_end",
        "spec:
  execution: {timeout: 1h, cleanup_timeout: 10m}
  steps:
    - {id: hold, wait: {for: 5m}}
    - {id: sig, wait: {signal: deploy, timeout: 15m}, on_failure: continue}
    - id: stop
      if: \"${{ steps.sig.status == 'timed_out' }}\"
      end: {status: failed, message: no signal}
    - {id: last, title: Last, actor: human}
  finally: [{id: notify, title: Notify}]
",
    ),
];

#[derive(Clone, Debug)]
enum Action {
    Report {
        step: String,
        status: ReportStatus,
        verdict: Option<Verdict>,
        rate: Option<f64>,
        human: bool,
    },
    Claim {
        step: String,
        human: bool,
    },
    Release {
        step: String,
        expired: bool,
    },
    Checklist {
        step: String,
        ticks: Vec<bool>,
    },
    Vote {
        step: String,
        user: u32,
        option: String,
        eligible: bool,
    },
    Signal {
        name: String,
        id: String,
    },
    Override {
        step: String,
        verdict: Option<Verdict>,
    },
    Pause(bool),
    Cancel,
    Tick(i64),
}

fn human(n: u32) -> ActorRef {
    ActorRef::Human {
        user_id: format!("usr_{n:026}").parse().unwrap(),
    }
}
fn agent() -> ActorRef {
    ActorRef::Agent {
        principal_id: "svc".into(),
        session_id: "s1".into(),
        name: "prop-agent".into(),
        model: None,
        service_attribution: None,
    }
}
fn directory() -> runbook_spec::CompileContext {
    let mut directory = runbook_spec::CompileContext::default();
    for (name, n) in [("oncall", 1), ("leads", 2)] {
        directory
            .teams
            .insert(name.into(), format!("team_{n:026}").parse().unwrap());
    }
    directory
}

struct Run {
    trace: Trace,
    now: i64,
}
impl Run {
    fn start(spec: &str) -> Self {
        let mut trace = run_document(&format!("{HEADER}{spec}"), &[], directory());
        assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
        trace.team_members.insert(
            "team_00000000000000000000000001".into(),
            (1..=3)
                .map(|n| format!("usr_{n:026}").parse().unwrap())
                .collect(),
        );
        Self { trace, now: 0 }
    }
    fn append(&mut self, event: Event, actor: ActorRef) -> Result<(), String> {
        let envelope = EventEnvelope {
            execution_id: self.trace.state.id.clone(),
            seq: self.trace.state.sequence + 1,
            at_ms: self.now,
            actor,
            event,
        };
        let next = apply(&self.trace.state, &envelope).map_err(|e| e.code().to_string())?;
        self.trace.state = next;
        self.trace.events.push(envelope);
        Ok(())
    }
    /// Like `Trace::settle`, but a `decide` error on a reachable state fails
    /// the property instead of being recorded.
    fn settle(&mut self, context: &str) {
        for _ in 0..1000 {
            let commands = decide(&self.trace.state)
                .unwrap_or_else(|e| panic!("{context}: decide failed: {e}"));
            assert_eq!(commands, decide(&self.trace.state).unwrap(), "{context}");
            if commands.is_empty() {
                return;
            }
            for command in commands {
                self.trace.commit_command(command, self.trace.state.at_ms);
            }
        }
        panic!("{context}: failed to quiesce");
    }
    fn run_id(&self, step: &str) -> Result<runbook_model::StepRunId, String> {
        self.trace
            .state
            .steps
            .iter()
            .find(|s| s.step_id == step)
            .and_then(|s| s.id.clone())
            .ok_or_else(|| "harness.no_run_id".into())
    }
    /// Applies one action; `Err` means it was rejected.
    fn act(&mut self, action: &Action) -> Result<(), String> {
        match action {
            Action::Tick(by) => {
                self.now += by;
                for _ in 0..1000 {
                    let commands = advance_time(&self.trace.state, self.now)
                        .map_err(|e| format!("advance_time failed: {e}"))
                        .unwrap();
                    if commands.is_empty() {
                        return Ok(());
                    }
                    for command in commands {
                        self.trace.commit_command(command, self.now);
                    }
                }
                panic!("time did not quiesce");
            }
            Action::Report {
                step,
                status,
                verdict,
                rate,
                human: by_human,
            } => {
                let mut outputs = Data::new();
                if let Some(rate) = rate {
                    outputs.insert("rate".into(), json!(rate));
                }
                let event = Event::StepReported {
                    step_run_id: self.run_id(step)?,
                    report: WorkReport {
                        appended_logs: false,
                        files: vec![],
                        status: *status,
                        verdict: *verdict,
                        outputs,
                        summary: (*status == ReportStatus::Skipped).then(|| "not needed".into()),
                        evidence: vec![],
                        logs: vec![],
                    },
                };
                self.append(event, if *by_human { human(1) } else { agent() })
            }
            Action::Claim { step, human: h } => {
                let event = Event::StepClaimed {
                    step_run_id: self.run_id(step)?,
                    agent_version_id: None,
                    claim_id: None,
                };
                self.append(event, if *h { human(2) } else { agent() })
            }
            Action::Release { step, expired } => {
                let step_run_id = self.run_id(step)?;
                if *expired {
                    self.append(Event::StepClaimExpired { step_run_id }, ActorRef::System)
                } else {
                    let actor = self.trace.step(step).actor.clone().unwrap_or(agent());
                    self.append(
                        Event::StepClaimReleased {
                            step_run_id,
                            reason: None,
                        },
                        actor,
                    )
                }
            }
            Action::Checklist { step, ticks } => {
                let event = update_checklist(&self.trace.state, &self.run_id(step)?, ticks)
                    .map_err(|e| e.code().to_string())?;
                self.append(event, human(1))
            }
            Action::Vote {
                step,
                user,
                option,
                eligible,
            } => {
                let vote = ResolvedVote {
                    at_ms: self.now,
                    base_sequence: self.trace.state.sequence,
                    step_run_id: self.run_id(step)?,
                    actor: human(*user),
                    credential_ref: format!("tok_{user}"),
                    option: option.clone(),
                    comment: None,
                    eligibility: VoteEligibility {
                        eligible: *eligible,
                        directory_revision: 1,
                        matched_recipients: if *eligible {
                            vec![RecipientRef::Team("team_00000000000000000000000001".into())]
                        } else {
                            vec![]
                        },
                    },
                };
                let events =
                    apply_vote(&self.trace.state, &vote).map_err(|e| e.code().to_string())?;
                for event in events {
                    self.append(event, vote.actor.clone())?;
                }
                Ok(())
            }
            Action::Signal { name, id } => {
                let input = WaitInput::Signal {
                    execution_id: self.trace.state.id.clone(),
                    signal_id: id.clone(),
                    name: name.clone(),
                    payload: Data::new(),
                    at_ms: self.now,
                };
                let commands =
                    advance_wait(&self.trace.state, &input).map_err(|e| e.code().to_string())?;
                for command in commands {
                    let Command::ConsumeSignal { signal } = command else {
                        panic!("advance_wait returned a non-signal command")
                    };
                    self.append(Event::SignalReceived { signal }, human(1))?;
                }
                Ok(())
            }
            Action::Override { step, verdict } => {
                let step_run_id = self.run_id(step)?;
                let current = current_record(self.trace.step(step));
                let to = StepRecord {
                    verdict: *verdict,
                    ..current
                };
                let amendment = validate_override(
                    &self.trace.state,
                    &step_run_id,
                    &to,
                    "corrected by admin",
                    human(3),
                )
                .map_err(|e| e.code().to_string())?;
                self.append(
                    Event::StepOverridden {
                        step_run_id,
                        amendment: Box::new(amendment),
                    },
                    human(3),
                )
            }
            Action::Pause(paused) => {
                self.append(Event::ManualPauseChanged { paused: *paused }, human(1))
            }
            Action::Cancel => self.append(
                Event::CancellationRequested {
                    superseded_by: None,
                    started_by: None,
                },
                human(1),
            ),
        }
    }
}

fn generate(run: &Run, rng: &mut Rng) -> Action {
    let state = &run.trace.state;
    let any_step = || {
        state
            .steps
            .iter()
            .map(|s| s.step_id.clone())
            .collect::<Vec<_>>()
    };
    let mut options: Vec<Action> = vec![];
    for step in &state.steps {
        let Some(node) = state.node(&step.step_id) else {
            continue;
        };
        match (&node.kind, step.status) {
            (NodeKind::Task(task), StepStatus::Ready | StepStatus::Claimed) => {
                for _ in 0..3 {
                    let status = *rng.pick(&[
                        ReportStatus::Succeeded,
                        ReportStatus::Succeeded,
                        ReportStatus::Succeeded,
                        ReportStatus::Failed,
                        ReportStatus::Blocked,
                        ReportStatus::Skipped,
                    ]);
                    let verdict = if task.check && status == ReportStatus::Succeeded {
                        Some(*rng.pick(&[
                            Verdict::Pass,
                            Verdict::Pass,
                            Verdict::Warn,
                            Verdict::Fail,
                        ]))
                    } else {
                        None
                    };
                    options.push(Action::Report {
                        step: step.step_id.clone(),
                        status,
                        verdict,
                        // Only `check` declares the output; occasionally
                        // send a wrong shape so closed outputs are exercised.
                        rate: (step.node_id == "check" || rng.chance(5))
                            .then(|| *rng.pick(&[0.0, 0.005, 0.02])),
                        human: rng.chance(50),
                    });
                }
                if !task.checklist.is_empty() {
                    options.push(Action::Checklist {
                        step: step.step_id.clone(),
                        ticks: vec![true; task.checklist.len()],
                    });
                }
                options.push(if step.status == StepStatus::Ready {
                    Action::Claim {
                        step: step.step_id.clone(),
                        human: rng.chance(50),
                    }
                } else {
                    Action::Release {
                        step: step.step_id.clone(),
                        expired: rng.chance(50),
                    }
                });
            }
            (NodeKind::Approval(approval), StepStatus::Waiting | StepStatus::Expired) => {
                for _ in 0..1 {
                    options.push(Action::Vote {
                        step: step.step_id.clone(),
                        user: 1 + rng.below(3) as u32,
                        option: rng.pick(&approval.options).id.clone(),
                        eligible: rng.chance(85),
                    });
                }
            }
            (NodeKind::Wait(wait), StepStatus::Waiting) => {
                if let WaitKind::Signal(name) = &wait.kind {
                    options.push(Action::Signal {
                        name: name.clone(),
                        id: format!("sig{}", rng.below(4)),
                    });
                }
            }
            (NodeKind::Task(_), StepStatus::Succeeded) if rng.chance(4) => {
                options.push(Action::Override {
                    step: step.step_id.clone(),
                    verdict: Some(Verdict::Pass),
                })
            }
            _ => {}
        }
    }
    if state.pause_reasons.contains(&PauseReason::Manual) {
        options.push(Action::Pause(false));
        options.push(Action::Pause(false));
    }
    let roll = rng.below(100);
    if roll < 12 || options.is_empty() {
        return Action::Tick(*rng.pick(&[1_000, 60_000, 120_000, 300_000, 600_000, 1_800_000]));
    }
    if roll < 15 {
        return Action::Pause(true);
    }
    if roll < 17 {
        return Action::Cancel;
    }
    if roll < 30 {
        // Noise: plausible actions aimed at arbitrary steps.
        let steps = any_step();
        let step = rng.pick(&steps).clone();
        return match rng.below(6) {
            0 => Action::Report {
                step,
                status: ReportStatus::Succeeded,
                verdict: None,
                rate: None,
                human: true,
            },
            1 => Action::Claim { step, human: false },
            2 => Action::Vote {
                step,
                user: 1,
                option: "go".into(),
                eligible: true,
            },
            3 => Action::Signal {
                name: "nobody-waits".into(),
                id: format!("n{}", rng.below(3)),
            },
            4 => Action::Override {
                step,
                verdict: Some(Verdict::Fail),
            },
            _ => Action::Release {
                step,
                expired: false,
            },
        };
    }
    options.swap_remove(rng.below(options.len()))
}

const CLOSED: [ExecutionStatus; 3] = [
    ExecutionStatus::Succeeded,
    ExecutionStatus::Failed,
    ExecutionStatus::Cancelled,
];

/// Generates and applies actions; returns them with their acceptance.
fn explore(spec: &str, seed: u64, length: usize) -> (Run, Vec<(Action, bool)>) {
    let mut rng = Rng(seed);
    let mut run = Run::start(spec);
    let checkpoints: Vec<usize> = (0..3).map(|_| rng.below(length)).collect();
    let mut log = vec![];
    for step in 0..length {
        let action = generate(&run, &mut rng);
        let closed = CLOSED.contains(&run.trace.state.status);
        let before = run.trace.events.len();
        let accepted = run.act(&action).is_ok();
        if !accepted {
            assert_eq!(run.trace.events.len(), before, "rejected action appended");
        }
        run.settle(&format!("seed {seed} step {step} {action:?}"));
        // Overrides amend completed records by design (PRD section 6).
        if closed && !matches!(action, Action::Tick(_) | Action::Override { .. }) {
            assert_eq!(
                run.trace.events.len(),
                before,
                "seed {seed} step {step}: closed execution accepted {action:?}"
            );
        }
        if CLOSED.contains(&run.trace.state.status) {
            let open: Vec<_> = run
                .trace
                .state
                .steps
                .iter()
                .filter(|s| {
                    matches!(
                        s.status,
                        StepStatus::Ready | StepStatus::Claimed | StepStatus::Waiting
                    )
                })
                .map(|s| (s.step_id.as_str(), s.status))
                .collect();
            assert!(
                open.is_empty(),
                "seed {seed} step {step}: closed run holds open work {open:?}"
            );
        }
        if checkpoints.contains(&step) {
            let replayed = replay_stored(
                run.trace.state.compiled(),
                &run.trace.state.id,
                &run.trace.events,
            );
            assert_eq!(
                replayed, run.trace.state,
                "seed {seed}: prefix replay at {step}"
            );
        }
        log.push((action, accepted));
    }
    (run, log)
}

fn rerun(spec: &str, actions: impl Iterator<Item = Action>) -> Run {
    let mut run = Run::start(spec);
    for (step, action) in actions.enumerate() {
        run.act(&action)
            .unwrap_or_else(|code| panic!("rerun step {step} rejected {action:?}: {code}"));
        run.settle("rerun");
    }
    run
}

#[test]
fn random_sequences_replay_deterministically() {
    // RUNBOOK_PROPERTY_SEEDS raises the count for a deeper local search.
    let seeds: u64 = std::env::var("RUNBOOK_PROPERTY_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    const LENGTH: usize = 50;
    let mut outcomes = std::collections::BTreeMap::<String, usize>::new();
    let mut rejected = 0;
    let mut kinds = std::collections::BTreeMap::<&str, usize>::new();
    for (name, spec) in RUNBOOKS {
        for seed in 0..seeds {
            let seed = seed * 7919 + name.len() as u64;
            let (run, log) = explore(spec, seed, LENGTH);
            let events = serde_json::to_vec(&run.trace.events).unwrap();
            assert_eq!(run.trace.state.sequence, run.trace.events.len() as u64);
            assert_eq!(
                replay_stored(
                    run.trace.state.compiled(),
                    &run.trace.state.id,
                    &run.trace.events
                ),
                run.trace.state,
                "{name} seed {seed}: full replay"
            );

            let mut again = Run::start(spec);
            for (action, accepted) in &log {
                assert_eq!(again.act(action).is_ok(), *accepted, "{name} seed {seed}");
                again.settle("again");
            }
            assert_eq!(
                serde_json::to_vec(&again.trace.events).unwrap(),
                events,
                "{name} seed {seed}: replay is not byte-identical"
            );

            let only_accepted = rerun(
                spec,
                log.iter().filter(|(_, ok)| *ok).map(|(a, _)| a.clone()),
            );
            assert_eq!(
                serde_json::to_vec(&only_accepted.trace.events).unwrap(),
                events,
                "{name} seed {seed}: a rejected action changed later behaviour"
            );

            rejected += log.iter().filter(|(_, ok)| !ok).count();
            for e in &run.trace.events {
                *kinds.entry(e.event.name()).or_default() += 1;
            }
            *outcomes
                .entry(format!("{name}:{:?}", run.trace.state.status))
                .or_default() += 1;
        }
    }
    // The generator must keep reaching the critical paths and producing
    // rejections, or the properties above prove little. Seeds are fixed, so
    // these counts are deterministic.
    assert!(rejected > 100, "only {rejected} rejected actions");
    for kind in [
        "ApprovalDecided",
        "ApprovalEscalated",
        "ApprovalExpired",
        "CatchActivated",
        "CleanupStarted",
        "CleanupSuppressed",
        "DecisionApplied",
        "ExecutionDeadlineElapsed",
        "ForeachStarted",
        "GroupFinallyStarted",
        "LoopIterationStarted",
        "RetryScheduled",
        "SiblingsCancelled",
        "SignalReceived",
        "StepClaimExpired",
        "StepHandedOff",
        "StepOverridden",
        "TaskDeadlineElapsed",
    ] {
        assert!(
            kinds.contains_key(kind),
            "generator never reached {kind}: {kinds:?}"
        );
    }
    for (name, _) in RUNBOOKS {
        let closed = CLOSED
            .iter()
            .map(|s| outcomes.get(&format!("{name}:{s:?}")).copied().unwrap_or(0))
            .sum::<usize>();
        assert!(closed > 0, "{name} never closed: {outcomes:?}");
    }
}
