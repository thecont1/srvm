use anyhow::Result;

fn main() -> Result<()> {
    let outcome = srvm::cli::run();
    // Ctrl+C already decided the exit code: an app that died because of the
    // interrupt must not turn an interrupted run into a clean exit.
    if srvm::supervise::shutdown_requested() {
        std::process::exit(130);
    }
    match outcome {
        Ok(()) => Ok(()),
        // Exit honesty (TECH-SPEC): propagate the child's own nonzero code so
        // a supervising script can tell "the app exited 7" from "srvm failed".
        Err(err) => {
            if let Some(code) = srvm::supervise::take_child_exit_code() {
                eprintln!("Error: {err:#}");
                std::process::exit(code);
            }
            Err(err)
        }
    }
}
