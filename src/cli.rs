use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use clap::Parser;

use crate::{
    detect::{PathResolver, PortInjection, ServeSpec, ToolResolver, detect},
    ports,
    runtime::{self, RuntimeKind},
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
        return print_dry_run(&root, &specs, cli.port);
    }

    if specs.is_empty() {
        bail!("no servable app detected in {}", root.display());
    }

    let selected = select_spec(&specs, cli.select.as_deref())?;
    print_intro(&root.display().to_string(), selected);
    let path_prepend = prepare_runtimes(&root, selected, cli.quiet)?;
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
        &path_prepend,
    )
}

fn print_intro(root: &str, spec: &ServeSpec) {
    println!("  srvm {}", env!("CARGO_PKG_VERSION"));
    println!("  workspace  {root}");
    println!("  serve      {}", spec.summary());
}

fn print_dry_run(root: &Path, specs: &[ServeSpec], port: Option<u16>) -> Result<()> {
    println!("  srvm {}", env!("CARGO_PKG_VERSION"));
    println!("  workspace  {}", root.display());

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
        print_runtime_note(root, spec);

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

fn print_runtime_note(root: &Path, spec: &ServeSpec) {
    let mut seen = Vec::new();
    for program in programs_of(spec) {
        if program_file(root, program).is_some() {
            continue;
        }
        let tool = bare_tool(program);
        if PathResolver.resolve(&tool, root).is_some() {
            continue;
        }
        let Some(kind) = runtime::kind_for(&tool) else {
            continue;
        };
        let label = match kind {
            RuntimeKind::Node => "node",
            RuntimeKind::Python => "python",
            RuntimeKind::Go => "go",
            RuntimeKind::Rust => "rust",
        };
        if seen.contains(&label) {
            continue;
        }
        seen.push(label);
        println!("  runtime    {label} (will fetch on launch)");
    }
}

fn prepare_runtimes(root: &Path, spec: &ServeSpec, quiet: bool) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let mut seen = Vec::new();
    for program in programs_of(spec) {
        if let Some(file) = program_file(root, program) {
            if let Some(dir) = file.parent() {
                push_unique(&mut dirs, dir.to_path_buf());
            }
            continue;
        }
        let tool = bare_tool(program);
        if let Some(resolved) = PathResolver.resolve(&tool, root) {
            if let Some(dir) = resolved.parent()
                && !on_path(dir)
            {
                push_unique(&mut dirs, dir.to_path_buf());
            }
            continue;
        }
        let Some(kind) = runtime::kind_for(&tool) else {
            continue;
        };
        if seen.contains(&kind) {
            continue;
        }
        seen.push(kind);
        dirs.extend(runtime::fetch_if_missing(kind, root, quiet)?);
    }
    Ok(dirs)
}

fn programs_of(spec: &ServeSpec) -> Vec<&str> {
    let mut programs = vec![spec.command.program.as_str()];
    if let Some(install) = &spec.install {
        programs.push(install.program.as_str());
    }
    programs
}

fn program_file(root: &Path, program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if !path.is_absolute() && path.components().count() < 2 {
        return None;
    }
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    candidate.is_file().then_some(candidate)
}

fn on_path(dir: &Path) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    env::split_paths(&path).any(|entry| entry.canonicalize().unwrap_or(entry) == dir)
}

fn push_unique(dirs: &mut Vec<PathBuf>, dir: PathBuf) {
    if !dirs.contains(&dir) {
        dirs.push(dir);
    }
}

fn bare_tool(program: &str) -> String {
    let name = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    name.trim_end_matches(".exe")
        .trim_end_matches(".cmd")
        .trim_end_matches(".bat")
        .to_string()
}

fn override_description(spec: &ServeSpec) -> String {
    match &spec.port {
        PortInjection::Env(key) => format!("env {key}=<port>"),
        PortInjection::Args(template) => format!("args {}", template.join(" ")),
        PortInjection::Listener => "built-in loopback listener (no child process)".into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::CommandSpec;
    use std::fs;

    fn spec_with_program(program: &str) -> ServeSpec {
        ServeSpec::new(
            "test",
            "test",
            CommandSpec::new(program, Vec::<String>::new()),
            None,
            PortInjection::None,
        )
    }

    #[test]
    fn repo_local_program_prepends_its_own_bin_dir() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join(".venv").join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("python"), "#!/bin/sh\n").unwrap();

        let spec = spec_with_program(".venv/bin/python");
        let dirs = prepare_runtimes(root.path(), &spec, true).unwrap();
        assert_eq!(dirs, vec![bin]);
    }

    #[test]
    fn bare_named_file_in_repo_is_not_a_program_path() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("npm"), "echo nope\n").unwrap();
        assert!(program_file(root.path(), "npm").is_none());
        assert!(
            program_file(root.path(), "./npm").is_some(),
            "relative ./npm resolves inside the repo"
        );
    }
}
