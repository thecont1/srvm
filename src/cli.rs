use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;

use crate::{
    detect::{PortInjection, ServeSpec, detect},
    ports,
    supervise::{self, SupervisorOptions},
};

#[derive(Debug, Parser)]
#[command(name = "srvm", version, about = "Zero-config universal app launcher")]
struct Cli {
    #[arg(default_value = ".")]
    dir: PathBuf,

    #[arg(long)]
    dry_run: bool,

    #[arg(long)]
    no_open: bool,

    #[arg(long, help = "Start port search at N (0 chooses a free port)")]
    port: Option<u16>,

    #[arg(long)]
    select: Option<String>,

    #[arg(long)]
    no_install: bool,

    #[arg(short = 'v', long)]
    verbose: bool,

    #[arg(long)]
    quiet: bool,

    #[arg(long)]
    no_color: bool,

    #[arg(long)]
    all: bool,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.dir.canonicalize()?;

    if cli.all {
        bail!("--all is reserved for multi-stack launch and is not yet supported");
    }

    let specs = detect(&root)?;

    if cli.dry_run {
        return print_dry_run(&root.display().to_string(), &specs, cli.port);
    }

    if specs.is_empty() {
        bail!("no servable app detected in {}", root.display());
    }

    let selected = select_spec(&specs, cli.select.as_deref())?;
    print_intro(&root.display().to_string(), selected);
    supervise::run(
        &root,
        selected,
        SupervisorOptions {
            no_open: cli.no_open,
            no_install: cli.no_install,
            verbose: cli.verbose,
            quiet: cli.quiet,
            no_color: cli.no_color,
            port: cli.port,
        },
    )
}

fn print_intro(root: &str, spec: &ServeSpec) {
    println!("  srvm {}", env!("CARGO_PKG_VERSION"));
    println!("  workspace  {root}");
    println!("  serve      {}", spec.summary());
}

fn print_dry_run(root: &str, specs: &[ServeSpec], port: Option<u16>) -> Result<()> {
    println!("  srvm {}", env!("CARGO_PKG_VERSION"));
    println!("  workspace  {root}");

    if specs.is_empty() {
        println!("  detect     no servable app detected");
        println!(
            "  looked     deno, package.json, wrangler, Procfile, make, just, task, python, ruby, docs, elixir, php, rust, go, compose, static"
        );
        return Ok(());
    }

    for (idx, spec) in specs.iter().enumerate() {
        println!("  match      {}. {}", idx + 1, spec.name);
        println!("  command    {}", spec.command_line());
        if let Some(install) = &spec.install {
            println!("  install    {}", install.command_line());
        }

        let inherited = match &spec.port {
            PortInjection::Env(key) => std::env::var(key).ok(),
            _ => None,
        };
        match ports::requested_port(spec, port, inherited.as_deref())? {
            Some(start) => {
                if start == 0 {
                    println!("  port       0 (OS-assigned free port chosen at launch)");
                } else {
                    println!("  port       {start} (start; availability checked at launch)");
                }
                println!("  override   {}", override_description(spec));
            }
            None => {
                if let Some(hint) = spec.url_hint {
                    println!("  port       {hint}");
                }
                if port.is_some() && matches!(spec.port, PortInjection::None) {
                    println!("  override   {}", override_description(spec));
                }
            }
        }
    }
    Ok(())
}

fn override_description(spec: &ServeSpec) -> String {
    match &spec.port {
        PortInjection::Env(key) => format!("env {key}=<port>"),
        PortInjection::Args(template) => format!("args {}", template.join(" ")),
        PortInjection::None => "unsupported — ports left unchanged".into(),
    }
}

fn select_spec<'a>(specs: &'a [ServeSpec], select: Option<&str>) -> Result<&'a ServeSpec> {
    match select {
        None => Ok(&specs[0]),
        Some(raw) => {
            if let Ok(n) = raw.parse::<usize>()
                && (1..=specs.len()).contains(&n)
            {
                return Ok(&specs[n - 1]);
            }
            specs
                .iter()
                .find(|spec| spec.name == raw || spec.tool == raw)
                .ok_or_else(|| anyhow::anyhow!("no detected stack matches --select {raw:?}"))
        }
    }
}
