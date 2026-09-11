//! The parsed form of a list expression.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOp {
    /// `|` or `+` - everything in either side, duplicates collapsed.
    Union,
    /// `-` - everything in the left side that is not in the right side.
    Difference,
    /// `&` - only what appears in both sides.
    Intersection,
    /// `^` - what appears in exactly one of the two sides.
    SymmetricDifference,
}

impl SetOp {
    pub fn symbol(self) -> &'static str {
        match self {
            SetOp::Union => "|",
            SetOp::Difference => "-",
            SetOp::Intersection => "&",
            SetOp::SymmetricDifference => "^",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// A source or another list, by name.
    Name(String),
    Op {
        op: SetOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
}

impl Expr {
    pub fn binary(op: SetOp, lhs: Expr, rhs: Expr) -> Expr {
        Expr::Op {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    /// Every name referenced by the expression, in order of first appearance.
    pub fn names(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_names(&mut out);
        out
    }

    fn collect_names<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Expr::Name(name) => {
                if !out.contains(&name.as_str()) {
                    out.push(name);
                }
            }
            Expr::Op { lhs, rhs, .. } => {
                lhs.collect_names(out);
                rhs.collect_names(out);
            }
        }
    }

    /// Fully bracketed rendering, used in error messages and tests.
    pub fn to_canonical_string(&self) -> String {
        match self {
            Expr::Name(name) => name.clone(),
            Expr::Op { op, lhs, rhs } => format!(
                "({} {} {})",
                lhs.to_canonical_string(),
                op.symbol(),
                rhs.to_canonical_string()
            ),
        }
    }
}
