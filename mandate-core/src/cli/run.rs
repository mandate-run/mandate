/// One `mandate run`.
pub struct Command {
    pub mandate_path: std::path::PathBuf,
    pub manifest_path: std::path::PathBuf,
    pub ledger_path: std::path::PathBuf,
    pub transcript_path: std::path::PathBuf,
    pub scenario: crate::fixture::Scenario,
    pub json: bool,
}