/// One `mandate reconcile`.
pub struct Command {
    pub mandate_id: String,
    pub mandate_path: std::path::PathBuf,
    pub ledger_path: std::path::PathBuf,
    pub json: bool,
}