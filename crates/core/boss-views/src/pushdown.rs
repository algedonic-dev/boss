//! Splitting a filter into "what SQL can answer" and a residual.
//!
//! # The contract
//!
//! Pushdown is an **optimization and only an optimization**. Whatever
//! SQL returns, the full original predicate is re-evaluated against
//! every row in-process before it reaches the caller. So the emitted
//! condition must be **implied by** the filter — a superset the
//! residual then trims. Never a subset, and never something the
//! database refuses to run.
//!
//! - An extractor that finds nothing is correct, just slow.
//! - An extractor that finds a term is correct, just faster.
//! - An extractor that finds a term *wrongly* is the only way to be
//!   wrong. Two ways to get that wrong, both learned the hard way:
//!   emitting a **narrower** condition than the filter (rows vanish
//!   with no way to recover them), or emitting one Postgres **refuses
//!   to type** (the request 500s instead of answering).
//!
//! # Why AND and OR differ
//!
//! Under AND, dropping a branch widens: `A` alone returns a superset
//! of `A AND B`, and the residual trims it. So a conjunct pushes down
//! whether or not its sibling does.
//!
//! Under OR, dropping a branch *narrows* — rows satisfying only the
//! dropped branch are lost, and the residual cannot get them back
//! because SQL never returned them. So a disjunction pushes down only
//! if **every** branch does. Approximating the unexpressible branch as
//! TRUE is sound but collapses the whole OR to TRUE, which is the same
//! as pushing nothing.
//!
//! That asymmetry is the entire subtlety here, and it is why
//! [`Pushdown::Or`] is built only from a complete set of branches.
//!
//! # Why NOT is never pushed
//!
//! Negation inverts the direction of approximation: if `A'` is a
//! superset of `A`, then `NOT A'` is a *subset* of `NOT A`. Pushing a
//! negated term is therefore sound only when the inner condition is
//! **exact**, and this extractor cannot promise exactness — the
//! in-process evaluator has its own semantics for missing fields and
//! type mismatches that need not agree with SQL's for every value. So
//! `NOT` stops the walk. Widening this needs an exactness proof, not
//! just a new match arm.

use boss_expr::{BinaryOp, Expr, UnaryOp, Value};

/// A condition SQL can serve, as a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pushdown {
    /// `<column> = <text literal>`.
    Eq {
        /// A projection column, resolved through the source's declared
        /// mapping — never caller text.
        column: &'static str,
        value: String,
    },
    All(Vec<Pushdown>),
    Any(Vec<Pushdown>),
}

impl Pushdown {
    /// Number of leaf comparisons — what `pushed_down` reports.
    pub fn term_count(&self) -> usize {
        match self {
            Pushdown::Eq { .. } => 1,
            Pushdown::All(cs) | Pushdown::Any(cs) => cs.iter().map(Self::term_count).sum(),
        }
    }

    /// Render to SQL, appending binds in the order they appear.
    ///
    /// `next_param` is the 1-based index of the next free placeholder,
    /// advanced as terms are emitted.
    pub fn to_sql(&self, next_param: &mut usize, binds: &mut Vec<String>) -> String {
        match self {
            Pushdown::Eq { column, value } => {
                binds.push(value.clone());
                let s = format!("{column} = ${next_param}");
                *next_param += 1;
                s
            }
            Pushdown::All(cs) => Self::join(cs, " AND ", next_param, binds),
            Pushdown::Any(cs) => Self::join(cs, " OR ", next_param, binds),
        }
    }

    fn join(
        parts: &[Pushdown],
        sep: &str,
        next_param: &mut usize,
        binds: &mut Vec<String>,
    ) -> String {
        let rendered: Vec<String> = parts.iter().map(|p| p.to_sql(next_param, binds)).collect();
        // Always parenthesised: an unparenthesised OR inside an AND
        // would bind wrong and silently widen the result.
        format!("({})", rendered.join(sep))
    }
}

/// Columns a source lets a filter push into SQL.
///
/// Maps the identifier a filter writer types to a column this crate
/// owns. Both halves are compile-time constants, which is what keeps
/// operator text out of the statement: a filter selects among these,
/// it can never name a new one.
///
/// **Every column listed here must be `TEXT`.** Only string literals
/// are pushed (see [`extract`]), so a non-text column would receive a
/// text bind and Postgres would refuse the comparison outright —
/// `operator does not exist: uuid = text` — turning a filter that used
/// to answer into a 500.
pub type PushableColumns = &'static [(&'static str, &'static str)];

/// Extract the largest condition SQL can answer, or `None`.
pub fn extract(expr: &Expr, pushable: PushableColumns) -> Option<Pushdown> {
    match expr {
        // AND: either branch alone still widens, so take what we can.
        Expr::BinaryOp(BinaryOp::And, lhs, rhs) => {
            match (extract(lhs, pushable), extract(rhs, pushable)) {
                (Some(a), Some(b)) => Some(Pushdown::All(vec![a, b])),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }
        // OR: all-or-nothing. A missing branch would narrow.
        Expr::BinaryOp(BinaryOp::Or, lhs, rhs) => {
            match (extract(lhs, pushable), extract(rhs, pushable)) {
                (Some(a), Some(b)) => Some(Pushdown::Any(vec![a, b])),
                _ => None,
            }
        }
        Expr::BinaryOp(BinaryOp::Eq, lhs, rhs) => as_eq(lhs, rhs, pushable),
        // Inequalities, NOT, function calls, bare terms: residual.
        Expr::BinaryOp(_, _, _)
        | Expr::UnaryOp(UnaryOp::Not, _)
        | Expr::FunctionCall(_, _)
        | Expr::Identifier(_)
        | Expr::Literal(_) => None,
    }
}

/// `<identifier> = <string literal>` in either order, where the
/// identifier names a pushable column.
fn as_eq(lhs: &Expr, rhs: &Expr, pushable: PushableColumns) -> Option<Pushdown> {
    let (path, value) = match (lhs, rhs) {
        (Expr::Identifier(p), Expr::Literal(v)) => (p, v),
        (Expr::Literal(v), Expr::Identifier(p)) => (p, v),
        _ => return None,
    };
    // Single-segment only. A dotted path reaches inside the JSON
    // payload, which has no column behind it.
    let [name] = path.as_slice() else {
        return None;
    };
    // Text literals only. Every pushable column is TEXT; binding an
    // int or a bool against one makes Postgres reject the statement
    // rather than the row, so a filter that used to answer 0 would
    // instead fail the request.
    let Value::String(s) = value else {
        return None;
    };
    let column = pushable
        .iter()
        .find(|(filter_name, _)| filter_name == name)
        .map(|(_, col)| *col)?;
    Some(Pushdown::Eq {
        column,
        value: s.clone(),
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

    fn ex(s: &str) -> Option<Pushdown> {
        extract(&boss_expr::parse(s).expect("parses"), EVENT_COLUMNS)
    }

    fn sql_of(s: &str) -> (String, Vec<String>) {
        let p = ex(s).expect("pushes down");
        let mut n = 1;
        let mut binds = Vec::new();
        let out = p.to_sql(&mut n, &mut binds);
        (out, binds)
    }

    #[test]
    fn a_bare_equality_pushes_down() {
        let (sql, binds) = sql_of("kind = \"products.consumed\"");
        assert_eq!(sql, "kind = $1");
        assert_eq!(binds, vec!["products.consumed".to_string()]);
    }

    #[test]
    fn literal_on_the_left_works_too() {
        let (sql, _) = sql_of("\"products.consumed\" = kind");
        assert_eq!(sql, "kind = $1");
    }

    #[test]
    fn an_or_of_two_pushable_branches_now_pushes() {
        // The case this extension exists for: previously it pushed
        // nothing, fell back to a capped scan, and reported 0 against
        // a true 16.
        let (sql, binds) = sql_of("kind = \"a\" OR kind = \"b\"");
        assert_eq!(sql, "(kind = $1 OR kind = $2)");
        assert_eq!(binds, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn an_or_with_one_unpushable_branch_pushes_nothing() {
        // THE soundness case. Pushing `kind = "a"` alone would drop
        // every row that matched only on the payload term, and the
        // residual could never recover them — SQL never returned them.
        assert_eq!(ex("kind = \"a\" OR amount = \"5\""), None);
        assert_eq!(ex("amount = \"5\" OR kind = \"a\""), None);
    }

    #[test]
    fn an_and_with_one_unpushable_branch_still_pushes_the_other() {
        // The dual: dropping a conjunct widens, which is safe.
        let (sql, binds) = sql_of("kind = \"a\" AND amount = \"5\"");
        assert_eq!(sql, "kind = $1");
        assert_eq!(binds, vec!["a".to_string()]);
    }

    #[test]
    fn an_or_nested_under_an_and_pushes_as_a_group() {
        let (sql, binds) = sql_of("subject_kind = \"account\" AND (kind = \"a\" OR kind = \"b\")");
        assert_eq!(sql, "(subject_kind = $1 AND (kind = $2 OR kind = $3))");
        assert_eq!(binds.len(), 3);
    }

    #[test]
    fn an_unpushable_branch_inside_a_nested_or_drops_only_that_or() {
        // The AND keeps its pushable half; the OR contributes nothing.
        let (sql, binds) =
            sql_of("subject_kind = \"account\" AND (kind = \"a\" OR amount = \"5\")");
        assert_eq!(sql, "subject_kind = $1");
        assert_eq!(binds, vec!["account".to_string()]);
    }

    #[test]
    fn or_groups_are_parenthesised_inside_an_and() {
        // Unparenthesised, `a AND b OR c` binds as `(a AND b) OR c`
        // and silently returns rows the filter excludes.
        let (sql, _) = sql_of("kind = \"a\" AND (subject_id = \"s\" OR subject_id = \"t\")");
        assert!(
            sql.contains("(subject_id = $2 OR subject_id = $3)"),
            "got: {sql}"
        );
    }

    #[test]
    fn not_is_never_pushed_even_when_its_inner_term_is_pushable() {
        // Negation inverts the direction of approximation; see the
        // module docs. Sound only with an exactness proof this
        // extractor does not have.
        assert_eq!(ex("NOT kind = \"a\""), None);
        assert_eq!(ex("NOT (kind = \"a\" OR kind = \"b\")"), None);
    }

    #[test]
    fn a_non_text_literal_never_pushes() {
        // Regression: these reached Postgres as `text = bigint` and
        // `uuid = text` and 500'd the request. Before pushdown existed
        // they evaluated in-process and returned nothing, gracefully.
        assert_eq!(ex("kind = 5"), None);
        assert_eq!(ex("kind = true"), None);
        assert_eq!(ex("subject_id = null"), None);
    }

    #[test]
    fn an_unmapped_or_dotted_field_does_not_push() {
        assert_eq!(ex("amount = \"5\""), None);
        assert_eq!(ex("payload.total = \"5\""), None);
    }

    #[test]
    fn inequality_does_not_push_down_yet() {
        // A legitimate future widening; today it is residual-only,
        // which is correct and merely slower.
        assert_eq!(ex("kind != \"a\""), None);
        assert_eq!(ex("kind > \"a\""), None);
    }

    #[test]
    fn term_count_reports_leaves_not_nodes() {
        assert_eq!(ex("kind = \"a\"").unwrap().term_count(), 1);
        assert_eq!(ex("kind = \"a\" OR kind = \"b\"").unwrap().term_count(), 2);
        assert_eq!(
            ex("subject_kind = \"x\" AND (kind = \"a\" OR kind = \"b\")")
                .unwrap()
                .term_count(),
            3
        );
    }

    #[test]
    fn placeholders_are_numbered_in_bind_order() {
        // Off-by-one here swaps two values between columns, which is a
        // wrong answer rather than a slow one.
        let (sql, binds) =
            sql_of("kind = \"k\" AND (subject_kind = \"sk\" OR subject_id = \"si\")");
        assert_eq!(
            sql,
            "(kind = $1 AND (subject_kind = $2 OR subject_id = $3))"
        );
        assert_eq!(
            binds,
            vec!["k".to_string(), "sk".to_string(), "si".to_string()]
        );
    }
}
