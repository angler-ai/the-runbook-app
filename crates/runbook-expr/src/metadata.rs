//! Conservative syntax dependencies. These never describe consumed runtime reads.
use cel_core::types::{Expr, SpannedExpr};

/// A root and optionally its first member. `None` includes the entire root;
/// `Some` includes the complete member subtree, regardless of dynamic indexes.
/// No execution identity, revision or runtime-read authority is attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticDependency {
    pub root: String,
    pub member: Option<String>,
}

// Called only after validate_parsed admits <=256 nodes and depth32. The stack
// and result are bounded by that tree; names are <=128 bytes. Dedup scans at
// most 256 borrowed pairs and copies only a new, bounded root/member pair.
pub(crate) fn dependencies(root: &SpannedExpr) -> Vec<StaticDependency> {
    let mut pending = vec![(root, None, None)];
    let mut found: Vec<(&str, Option<&str>)> = Vec::new();
    while let Some((node, member, alias)) = pending.pop() {
        match &node.node {
            Expr::Ident(name) => {
                if Some(name.as_str()) == alias {
                    continue;
                }
                let entry = (name.as_str(), member);
                if !found.contains(&entry) {
                    found.push(entry);
                }
            }
            Expr::Member { expr, field, .. } => pending.push((expr, Some(field.as_str()), alias)),
            Expr::Index { expr, index, .. } => {
                pending.push((index, None, alias));
                pending.push((expr, None, alias));
            }
            Expr::Unary { expr, .. } => pending.push((expr, None, alias)),
            Expr::Binary { left, right, .. } => {
                pending.push((right, None, alias));
                pending.push((left, None, alias));
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
            } => {
                pending.push((else_expr, None, alias));
                pending.push((then_expr, None, alias));
                pending.push((cond, None, alias));
            }
            Expr::Call { args, .. } => {
                for arg in args.iter().rev() {
                    pending.push((arg, None, alias));
                }
            }
            Expr::Comprehension(data) => {
                // validate_parsed admits only one canonical, nonnested exists.
                // Its range already covers every possible alias dependency;
                // generated accumulator nodes carry no caller binding reads.
                let Expr::Binary {
                    left: predicate, ..
                } = &data.loop_step.node
                else {
                    unreachable!("validated canonical exists step");
                };
                pending.push((predicate, None, Some(data.iter_var.as_str())));
                pending.push((&data.iter_range, None, alias));
            }
            Expr::Null | Expr::Bool(_) | Expr::Int(_) | Expr::Float(_) | Expr::String(_) => {}
            _ => unreachable!("profile rejects this AST before dependency collection"),
        }
    }
    found
        .into_iter()
        .map(|(root, member)| StaticDependency {
            root: root.into(),
            member: member.map(str::to_owned),
        })
        .collect()
}
