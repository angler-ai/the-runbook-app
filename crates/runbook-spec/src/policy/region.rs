//! Monotone before/after-decision transfer with distinct completion reasons.
//! Memoization bounds repeated traversal of nested loops and shared cleanup.
use crate::ir::*;
use std::collections::BTreeMap;
const BEFORE: u8 = 1;
const AFTER: u8 = 2;
#[derive(Clone, Copy, Default)]
struct Flow {
    normal: u8,
    failure: u8,
    end: u8,
}
impl Flow {
    fn normal(normal: u8) -> Self {
        Self {
            normal,
            ..Self::default()
        }
    }
    fn merge(&mut self, other: Self) {
        self.normal |= other.normal;
        self.failure |= other.failure;
        self.end |= other.end;
    }
    fn outcomes(self) -> [u8; 3] {
        [self.normal, self.failure, self.end]
    }
}
pub(super) struct Findings<'a> {
    pub auto: Option<&'a Node>,
    pub agent: Option<&'a Node>,
    pub memo_entries: usize,
}
struct Region<'a> {
    target: &'a str,
    auto: bool,
    agent: bool,
    auto_task: Option<&'a Node>,
    agent_task: Option<&'a Node>,
    memo: BTreeMap<(&'a str, u8), Flow>,
}
pub(super) fn analyze<'a>(
    ir: &'a NormalizedRunbook,
    target: &'a str,
    auto: bool,
    agent: bool,
) -> Findings<'a> {
    let mut region = Region {
        target,
        auto,
        agent,
        auto_task: None,
        agent_task: None,
        memo: BTreeMap::new(),
    };
    let ordinary = region.sequence(&ir.steps, BEFORE);
    region.cleanup(ordinary, &ir.finally);
    Findings {
        auto: region.auto_task,
        agent: region.agent_task,
        memo_entries: region.memo.len(),
    }
}
impl<'a> Region<'a> {
    fn sequence(&mut self, nodes: &'a [Node], input: u8) -> Flow {
        let mut flow = Flow::normal(input);
        for node in nodes {
            if flow.normal == 0 {
                break;
            }
            let next = self.node(node, flow.normal);
            flow.normal = next.normal;
            flow.failure |= next.failure;
            flow.end |= next.end;
        }
        flow
    }
    fn cleanup(&mut self, flow: Flow, nodes: &'a [Node]) -> Flow {
        let normal = self.sequence(nodes, flow.normal);
        let failure = self.sequence(nodes, flow.failure);
        let end = self.sequence(nodes, flow.end);
        Flow {
            normal: normal.normal,
            failure: normal.failure | failure.normal | failure.failure,
            end: normal.end | failure.end | end.normal | end.failure | end.end,
        }
    }
    fn iteration(&mut self, nodes: &'a [Node], input: u8, maximum: u32, empty: bool) -> Flow {
        let mut flow = self.sequence(nodes, input);
        if maximum >= 2 && flow.normal != 0 {
            // Before/after bits are monotone. A second traversal includes every
            // possible newly activated input; larger numeric bounds add no state.
            flow.merge(self.sequence(nodes, input | flow.normal));
        }
        if empty {
            flow.normal |= input;
        }
        // until / per-item key expressions can fail after body completion.
        flow.failure |= input | flow.normal;
        flow
    }
    fn node(&mut self, node: &'a Node, input: u8) -> Flow {
        if input == 0 {
            return Flow::default();
        }
        let key = (node.id.as_str(), input);
        if let Some(flow) = self.memo.get(&key) {
            return *flow;
        }
        let mut flow = match &node.kind {
            NodeKind::Task(task) => {
                if input & AFTER != 0 && task.destructive {
                    if self.auto && !task.allow_auto_decision && self.auto_task.is_none() {
                        self.auto_task = Some(node);
                    }
                    if self.agent && self.agent_task.is_none() {
                        self.agent_task = Some(node);
                    }
                }
                Flow {
                    normal: input,
                    failure: input,
                    end: 0,
                }
            }
            // LANG4: the child runs after this point, so a child that can
            // reach a destructive task is one (it has no allow_auto_decision).
            NodeKind::Call(call) => {
                if input & AFTER != 0 && call.target.as_ref().is_some_and(|t| t.destructive) {
                    if self.auto && self.auto_task.is_none() {
                        self.auto_task = Some(node);
                    }
                    if self.agent && self.agent_task.is_none() {
                        self.agent_task = Some(node);
                    }
                }
                // A child decision can govern success, failure, or cleanup
                // in the caller. Retain the path where the child never decided.
                let returned = if node.id == self.target {
                    input | AFTER
                } else {
                    input
                };
                Flow {
                    normal: returned,
                    failure: returned,
                    end: 0,
                }
            }
            NodeKind::Approval(_) => Flow {
                normal: if node.id == self.target { AFTER } else { input },
                failure: input,
                end: 0,
            },
            NodeKind::Wait(_) | NodeKind::Set(_) => Flow {
                normal: input,
                failure: input,
                end: 0,
            },
            NodeKind::End(_) => Flow {
                end: input,
                ..Flow::default()
            },
            NodeKind::Group(group) => {
                let mut body = self.sequence(&group.children, input);
                if !group.catch.is_empty() {
                    let caught = self.sequence(&group.catch, body.failure);
                    body.failure = 0;
                    body.merge(caught);
                }
                self.cleanup(body, &group.finally)
            }
            NodeKind::Switch(switch) => {
                let mut branches = self.sequence(&switch.default, input);
                for case in &switch.cases {
                    branches.merge(self.sequence(&case.children, input));
                }
                branches.failure |= input;
                branches
            }
            NodeKind::Parallel { children, .. } => {
                let mut joined = Flow::normal(input);
                for child in children {
                    // Each independent branch receives the original input. A
                    // decision made in one branch is only combined at the join.
                    joined = join(joined, self.node(child, input));
                }
                joined
            }
            NodeKind::Foreach(each) => self.iteration(&each.children, input, each.max_items, true),
            NodeKind::Loop(repeat) => {
                self.iteration(&repeat.children, input, repeat.max_iterations, false)
            }
        };
        if node.guard.is_some() {
            flow.normal |= input;
            flow.failure |= input;
        }
        if node.on_failure == FailurePolicy::Continue {
            flow.normal |= flow.failure;
            flow.failure = 0;
        }
        // Cancellation/deadline can stop entered work before or after a decision.
        // End remains a terminal channel through enclosing finally blocks.
        flow.end |= input | flow.normal | flow.failure;
        self.memo.insert(key, flow);
        flow
    }
}
fn join(left: Flow, right: Flow) -> Flow {
    let mut outcomes = [0; 3];
    for (l_kind, l_mask) in left.outcomes().into_iter().enumerate() {
        for (r_kind, r_mask) in right.outcomes().into_iter().enumerate() {
            for l in [BEFORE, AFTER] {
                for r in [BEFORE, AFTER] {
                    if l_mask & l != 0 && r_mask & r != 0 {
                        outcomes[l_kind.max(r_kind)] |= if l == AFTER || r == AFTER {
                            AFTER
                        } else {
                            BEFORE
                        };
                    }
                }
            }
        }
    }
    Flow {
        normal: outcomes[0],
        failure: outcomes[1],
        end: outcomes[2],
    }
}
