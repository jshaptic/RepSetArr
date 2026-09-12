//! The parsed form of a filter expression.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoolExpr {
    /// A named filter block.
    Name(String),
    Not(Box<BoolExpr>),
    And(Box<BoolExpr>, Box<BoolExpr>),
    Or(Box<BoolExpr>, Box<BoolExpr>),
}

impl BoolExpr {
    pub fn and(lhs: BoolExpr, rhs: BoolExpr) -> BoolExpr {
        BoolExpr::And(Box::new(lhs), Box::new(rhs))
    }

    pub fn or(lhs: BoolExpr, rhs: BoolExpr) -> BoolExpr {
        BoolExpr::Or(Box::new(lhs), Box::new(rhs))
    }

    pub fn negate(inner: BoolExpr) -> BoolExpr {
        BoolExpr::Not(Box::new(inner))
    }

    /// Every filter name referenced, in order of first appearance.
    pub fn names(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_names(&mut out);
        out
    }

    fn collect_names<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            BoolExpr::Name(name) => {
                if !out.contains(&name.as_str()) {
                    out.push(name);
                }
            }
            BoolExpr::Not(inner) => inner.collect_names(out),
            BoolExpr::And(lhs, rhs) | BoolExpr::Or(lhs, rhs) => {
                lhs.collect_names(out);
                rhs.collect_names(out);
            }
        }
    }

    /// Fully bracketed rendering, used in error messages and tests.
    pub fn to_canonical_string(&self) -> String {
        match self {
            BoolExpr::Name(name) => name.clone(),
            BoolExpr::Not(inner) => format!("(not {})", inner.to_canonical_string()),
            BoolExpr::And(lhs, rhs) => format!(
                "({} and {})",
                lhs.to_canonical_string(),
                rhs.to_canonical_string()
            ),
            BoolExpr::Or(lhs, rhs) => format!(
                "({} or {})",
                lhs.to_canonical_string(),
                rhs.to_canonical_string()
            ),
        }
    }
}
