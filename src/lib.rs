pub mod bootstrap;
pub mod cli;

/// Build the generated completion/man-page assets from the same schema the CLI parses.
pub fn command() -> clap::Command {
    cli::command()
}
pub mod detect;
pub mod dotenv;
pub mod launch;
pub mod ports;
pub mod runtime;
pub mod staticsrv;
pub mod supervise;
pub mod workspace;
