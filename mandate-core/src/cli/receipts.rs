/// One `mandate receipts`.
pub struct Command {
    pub mandate_id: String,
    pub ledger_path: std::path::PathBuf,
    pub json: bool,
}