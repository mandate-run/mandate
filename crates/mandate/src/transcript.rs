//! Spec section 12: the transcript. This file holds the quotes table; the
//! rest of the transcript arrives with the one-pool run.

use crate::mandate::format_amount;
use crate::quote::{Estimate, Quote};

/// The quotes table: listing, amount, ceiling, the three judgments and
/// latency, then ceiling estimates for steps not yet quotable. Amounts are
/// decimal strings in the asset's unit.
pub fn quotes_table(quotes: &[Quote], estimates: &[Estimate], decimals: u32) -> String {
    let mut out = String::from(
        "listing          amount      ceiling     within_tariff listing_match fee_payer_ok latency_ms\n",
    );
    for q in quotes {
        out.push_str(&format!(
            "{:<16} {:>11} {:>11} {:<13} {:<13} {:<12} {:>10}\n",
            q.listing_id,
            format_amount(q.amount, decimals),
            format_amount(q.ceiling, decimals),
            q.within_tariff,
            q.listing_match,
            q.fee_payer_ok,
            q.latency_ms
        ));
    }
    for e in estimates {
        out.push_str(&format!(
            "{:<16} {:>11} {:>11} estimate at ceiling for {} units\n",
            e.listing_id,
            "-",
            format_amount(e.ceiling, decimals),
            e.units
        ));
    }
    out
}
