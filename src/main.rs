use anyhow::Result;

fn main() -> Result<()> {
    let outcome = srvm::cli::run();
    // Ctrl+C already decided the exit code: an app that died because of the
    // interrupt must not turn an interrupted run into a clean exit.
    if srvm::supervise::shutdown_requested() {
        std::process::exit(130);
    }
    outcome
}
