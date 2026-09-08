//! Spec section 2.7: refusal reasons. Every purchase not made carries one.

/// The code printed in `REFUSED <code> ...` lines and written to receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Code {
    OverBudget,
    ReserveViolation,
    OutsideConstraints,
    OffTariff,
    EvidenceInsufficient,
    RequirementUnmeetable,
    SellerUnreachable,
    PaymentUnresolved,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OverBudget => "OVER_BUDGET",
            Self::ReserveViolation => "RESERVE_VIOLATION",
            Self::OutsideConstraints => "OUTSIDE_CONSTRAINTS",
            Self::OffTariff => "OFF_TARIFF",
            Self::EvidenceInsufficient => "EVIDENCE_INSUFFICIENT",
            Self::RequirementUnmeetable => "REQUIREMENT_UNMEETABLE",
            Self::SellerUnreachable => "SELLER_UNREACHABLE",
            Self::PaymentUnresolved => "PAYMENT_UNRESOLVED",
        }
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A refusal a human can read: the code and the numbers behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: Code,
    pub detail: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "REFUSED {} {}", self.code, self.detail)
    }
}
