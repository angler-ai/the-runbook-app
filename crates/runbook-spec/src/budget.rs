//! Conservative structural work count; no expressions are evaluated here.
use crate::Diagnostic;
use crate::ir::{Node, NodeKind, NormalizedRunbook};
use crate::normalize::{MAX_CONTROL_DEPTH, MAX_CONTROL_NODES, MAX_SOURCE_BYTES, MAX_SOURCE_VALUES};

/// Count leaf work from structural maxima, including cleanup and recovery.
/// Retries are attempts on one step run; guards never reduce this estimate.
/// This checks the tree's node/depth envelope again because intermediate IR is
/// mutable. It does not validate schemas, evaluate expressions or seal the IR.
pub fn estimate_step_runs(runbook: &NormalizedRunbook) -> Result<u64, Diagnostic> {
    let mut walker = Walker { nodes: 0, cases: 0 };
    let main = walker.body(&runbook.steps, 0, 0)?;
    walker.body(&runbook.finally, 0, main)
}
fn error(code: &str, path: &str) -> Diagnostic {
    // A caller may mutate public intermediate paths. Normalized node pointers
    // fit the source envelope; use the root for an oversized synthetic path.
    Diagnostic {
        message: None,
        code: code.into(),
        path: if path.len() <= MAX_SOURCE_BYTES {
            path.into()
        } else {
            String::new()
        },
        line: 0,
        column: 0,
    }
}
fn add(left: u64, right: u64, path: &str) -> Result<u64, Diagnostic> {
    left.checked_add(right)
        .ok_or_else(|| error("spec.step_budget_overflow", path))
}
struct Walker {
    nodes: usize,
    cases: usize,
}
impl Walker {
    // Recursion admits only depths 0..=32; sibling iteration allocates nothing.
    fn body(&mut self, nodes: &[Node], depth: usize, mut total: u64) -> Result<u64, Diagnostic> {
        for node in nodes {
            total = add(total, self.node(node, depth)?, &node.source_path)?;
        }
        Ok(total)
    }
    fn node(&mut self, node: &Node, depth: usize) -> Result<u64, Diagnostic> {
        let path = node.source_path.as_str();
        if depth > MAX_CONTROL_DEPTH {
            return Err(error("spec.control_depth_limit", path));
        }
        if self.nodes == MAX_CONTROL_NODES {
            return Err(error("spec.node_limit", path));
        }
        self.nodes += 1;
        match &node.kind {
            NodeKind::Task(_) | NodeKind::Approval(_) | NodeKind::Wait(_) | NodeKind::Call(_) => {
                Ok(1)
            }
            NodeKind::Set(_) | NodeKind::End(_) => Ok(0),
            NodeKind::Parallel { children, .. } => self.body(children, depth + 1, 0),
            NodeKind::Foreach(group) => self
                .body(&group.children, depth + 1, 0)?
                .checked_mul(u64::from(group.max_items))
                .ok_or_else(|| error("spec.step_budget_overflow", path)),
            NodeKind::Loop(group) => self
                .body(&group.children, depth + 1, 0)?
                .checked_mul(u64::from(group.max_iterations))
                .ok_or_else(|| error("spec.step_budget_overflow", path)),
            NodeKind::Switch(branch) => {
                // Empty case bodies have no nodes. Reserve their traversal from
                // the source-value ceiling before visiting any of these cases.
                self.cases = self
                    .cases
                    .checked_add(branch.cases.len())
                    .filter(|count| *count <= MAX_SOURCE_VALUES)
                    .ok_or_else(|| error("spec.source_value_limit", path))?;
                let mut maximum = 0;
                for case in &branch.cases {
                    maximum = maximum.max(self.body(&case.children, depth + 1, 0)?);
                }
                Ok(maximum.max(self.body(&branch.default, depth + 1, 0)?))
            }
            NodeKind::Group(group) => {
                let ordinary = self.body(&group.children, depth + 1, 0)?;
                let recovered = self.body(&group.catch, depth + 1, ordinary)?;
                self.body(&group.finally, depth + 1, recovered)
            }
        }
    }
}
