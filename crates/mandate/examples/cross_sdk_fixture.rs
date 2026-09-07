//! Writes the cross-SDK fixture to stdout. See `mandate::hedera::fixture`.
//!
//! ```text
//! cargo run -q -p mandate --example cross_sdk_fixture > sellers/fixtures/rust-signed-transfers.json
//! ```

fn main() {
    print!(
        "{}",
        mandate::hedera::fixture::render(&mandate::hedera::fixture::build())
    );
}
