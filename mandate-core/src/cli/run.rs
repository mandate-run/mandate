/// One `mandate run`.
pub struct Command {
    pub mandate_path: std::path::PathBuf,
    pub manifest_path: std::path::PathBuf,
    pub ledger_path: std::path::PathBuf,
    pub transcript_path: std::path::PathBuf,
    pub scenario: crate::fixture::Scenario,
    pub json: bool,
    /// Run against live x402 sellers, Hedera testnet settlement and HCS
    /// receipts instead of the fixture (config comes from the environment).
    pub live: bool,
    /// Base URL of the live seller fleet; listing URLs in the pinned manifest
    /// are rewritten to `{base}/{listing_id}`.
    pub sellers_url: Option<String>,
}
