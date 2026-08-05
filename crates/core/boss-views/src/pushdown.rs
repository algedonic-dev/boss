//! Splitting a filter into "what SQL can answer" and a residual.
//!
//! # The contract
//!
//! Pushdown is an **optimization and only an optimization**. Whatever
//! SQL returns, the full original predicate is re-evaluated against
//! every row in-process before it reaches the caller. So:
//!
//! - An extractor that finds nothing is correct, just slow.
//! - An extractor that finds a term is correct, just faster.
//! - An extractor that finds a term *wrongly* is the only way to be
//!   wrong, which is why the rules below are narrow and why every
//!   widening belongs with a test.
//!
//! That contract is what makes this a seam a query planner can grow
//! into rather than a rewrite. Today it understands equality against a
//! literal on an AND-spine. Teaching it ranges, `IN`, or OR-trees makes
//! queries faster; it cannot make them return different rows.
//!
//! # Why only the AND-spine
//!
//! A conjunct can be pushed down alone only if dropping it would widen
//! the result set rather than narrow it. That holds under AND — SQL
//! returns a superset, the residual trims it — and fails under OR and
//! NOT, where a term constrains nothing on its own. So the walk stops
//! at the first `OR`, `NOT`, or function call rather than descending
//! into it.

use boss_expr::{BinaryOp, Expr, UnaryOp, Value};

/// One equality constraint SQL can serve: `<column> = <literal>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Constraint {
    /// The projection column, already resolved from the filter's
    /// identifier path through the source's declared mapping. Never
    /// caller text — see [`extract`].
    pub column: &'static str,
    pub value: Value,
}

/// Columns a source lets a filter push into SQL.
///
/// The mapping is from the identifier path a filter writer types to a
/// column name this crate owns. Both halves are fixed at compile time,
/// which is what keeps operator text out of the statement: a filter can
/// only ever select which of these columns is constrained, never name a
/// new one.
pub type PushableColumns = &'static [(&'static str, &'static str)];

/// Walk the AND-spine and collect the equality constraints that map to
/// a pushable column.
///
/// Returns constraints in the order encountered. Duplicates and
/// contradictions (`kind = "a" AND kind = "b"`) are left alone: SQL will
/// return nothing for the contradiction, which is the right answer, and
/// the residual would have produced the same.
pub fn extract(expr: &Expr, pushable: PushableColumns) -> Vec<Constraint> {
    let mut out = Vec::new();
    collect(expr, pushable, &mut out);
    out
}

fn collect(expr: &Expr, pushable: PushableColumns, out: &mut Vec<Constraint>) {
    match expr {
        // The spine: both sides are still conjunctions of the whole.
        Expr::BinaryOp(BinaryOp::And, lhs, rhs) => {
            collect(lhs, pushable, out);
            collect(rhs, pushable, out);
        }
        Expr::BinaryOp(BinaryOp::Eq, lhs, rhs) => {
            if let Some(c) = as_constraint(lhs, rhs, pushable) {
                out.push(c);
            }
        }
        // Everything else — OR, NOT, comparisons other than equality,
        // function calls, bare identifiers — stays for the residual.
        // Descending into an OR or a NOT to grab a conjunct would
        // constrain rows the predicate does not.
        Expr::BinaryOp(_, _, _)
        | Expr::UnaryOp(UnaryOp::Not, _)
        | Expr::FunctionCall(_, _)
        | Expr::Identifier(_)
        | Expr::Literal(_) => {}
    }
}

/// `<identifier> = <literal>` in either order, where the identifier
/// names a pushable column.
fn as_constraint(lhs: &Expr, rhs: &Expr, pushable: PushableColumns) -> Option<Constraint> {
    let (path, value) = match (lhs, rhs) {
        (Expr::Identifier(p), Expr::Literal(v)) => (p, v),
        (Expr::Literal(v), Expr::Identifier(p)) => (p, v),
        _ => return None,
    };
    // Only single-segment paths. A dotted path reaches inside the JSON
    // payload, which has no column to push into.
    let [name] = path.as_slice() else {
        return None;
    };
    let column = pushable
        .iter()
        .find(|(filter_name, _)| filter_name == name)
        .map(|(_, col)| *col)?;
    // Null has no useful equality in SQL (`= NULL` is never true), so
    // leave it to the residual rather than emitting a term that would
    // silently match nothing.
    if matches!(value, Value::Null) {
        return None;
    }
    Some(Constraint {
        column,
        value: value.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT_COLUMNS: PushableColumns = &[
        ("kind", "kind"),
        ("source", "source"),
        ("subject_kind", "subject_kind"),
        ("subject_id", "subject_id"),
    ];

    fn parse(s: &str) -> Expr {
        boss_expr::parse(s).expect("test filter parses")
    }

    fn extracted(s: &str) -> Vec<Constraint> {
        extract(&parse(s), EVENT_COLUMNS)
    }

    #[test]
    fn a_bare_equality_pushes_down() {
        let c = extracted("kind = \"products.consumed\"");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].column, "kind");
        assert_eq!(c[0].value, Value::String("products.consumed".into()));
    }

    #[test]
    fn literal_on_the_left_works_too() {
        let c = extracted("\"products.consumed\" = kind");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].column, "kind");
    }

    #[test]
    fn the_and_spine_yields_every_conjunct() {
        let c = extracted(
            "kind = \"jobs.step.updated\" AND subject_kind = \"account\" \
             AND subject_id = \"acc-1\"",
        );
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].column, "kind");
        assert_eq!(c[1].column, "subject_kind");
        assert_eq!(c[2].column, "subject_id");
    }

    #[test]
    fn an_unmapped_field_does_not_push_down() {
        // `amount` lives in the payload. It has no column, so it is the
        // residual's problem — and the residual still applies it.
        assert!(extracted("amount = 5").is_empty());
    }

    #[test]
    fn a_dotted_path_does_not_push_down() {
        assert!(extracted("payload.total = 5").is_empty());
    }

    #[test]
    fn or_blocks_pushdown_entirely() {
        // THE correctness case. `kind = "a" OR subject_id = "s"` must
        // not push `kind = "a"`: that would drop every row matching
        // only the second branch, and the residual could never get
        // them back.
        assert!(
            extracted("kind = \"a\" OR subject_id = \"s\"").is_empty(),
            "a conjunct under OR constrains nothing on its own"
        );
    }

    #[test]
    fn an_or_nested_under_an_and_does_not_leak_its_branches() {
        // The AND-spine is walked, so `kind` pushes; the OR beneath it
        // contributes nothing.
        let c = extracted("kind = \"a\" AND (subject_id = \"s\" OR source = \"x\")");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].column, "kind");
    }

    #[test]
    fn not_blocks_pushdown() {
        assert!(extracted("NOT kind = \"a\"").is_empty());
    }

    #[test]
    fn inequality_does_not_push_down_yet() {
        // A range is a legitimate future widening; today it stays in
        // the residual, which is correct and merely slower.
        assert!(extracted("kind != \"a\"").is_empty());
        assert!(extracted("occurred_at > \"2026-01-01\"").is_empty());
    }

    #[test]
    fn null_equality_stays_in_the_residual() {
        // `col = NULL` matches nothing in SQL, which is not what the
        // in-process evaluator means by it.
        assert!(extracted("subject_id = null").is_empty());
    }

    #[test]
    fn contradictions_are_left_to_sql() {
        // Both terms push; SQL returns nothing; that is the answer.
        let c = extracted("kind = \"a\" AND kind = \"b\"");
        assert_eq!(c.len(), 2);
    }
}
