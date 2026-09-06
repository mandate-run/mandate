/// One `mandate init`.
pub struct Command {
    pub out_dir: std::path::PathBuf,
    pub principal: String,
    pub purpose: String,
    /// Atomic units of USDC (6 decimals).
    pub service_total: i128,
    /// Atomic units of HBAR (8 decimals).
    pub audit_total: i128,
}