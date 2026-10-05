use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use clap::Parser;

use crate::{
    detect::{PortInjection, ServeSpec, binpath},
    dotenv, launch, ports,
    runtime::{self, RuntimeKind, hint::Scope},
    supervise::{self, LaunchItem, SupervisorOptions},
    workspace::{self, Candidate},
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

    #[arg(long, help = "Run one app: index, qualified id, or a unique name")]
    select: Option<String>,

    #[arg(long)]
    no_install: bool,

    #[arg(short = 'v', long)]
    verbose: bool,

    #[arg(long)]
    quiet: bool,

    #[arg(long)]
    no_color: bool,

    #[arg(long, conflicts_with = "select", help = "Alias for the default set")]
    all: bool,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let workspace = workspace::discover(&cli.dir)?;

    for note in &workspace.notes {
        eprintln!("  note       {note}");
    }

    if cli.dry_run {
        return print_dry_run(&workspace, cli.port, cli.select.as_deref());
    }

    if workspace.candidates.is_empty() {
        bail!("no servable app detected in {}", workspace.root.display());
    }

    let options = SupervisorOptions {
        no_open: cli.no_open,
        no_install: cli.no_install,
        verbose: cli.verbose,
        quiet: cli.quiet,
        no_color: cli.no_color,
        port: cli.port,
    };

    let plan = launch::plan_default(&workspace.candidates);
    let (set, defaulted) = match cli.select.as_deref() {
        Some(select) => (vec![select_index(&workspace.candidates, select)?], false),
        None => (plan.set.clone(), true),
    };

    if defaulted && plan.orchestrated {
        note_orchestration(&workspace.candidates, &plan);
    }

    if set.len() == 1 {
        let candidate = &workspace.candidates[set[0]];
        print_intro(&workspace.root, candidate);
        let path_prepend = prepare_runtimes(candidate, &workspace.root, cli.quiet)?;
        let env = prepare_env(candidate);
        return supervise::run(
            &candidate.root,
            &candidate.spec,
            options,
            &path_prepend,
            &env,
        );
    }

    println!("  srvm {}", env!("CARGO_PKG_VERSION"));
    println!("  workspace  {}", workspace.root.display());

    let labels = labels_for(&set, &workspace.candidates);
    for (label, &index) in labels.iter().zip(&set) {
        println!(
            "  serve      [{label}] {}",
            workspace.candidates[index].spec.summary()
        );
    }

    // Runtime fetches stage under a shared cache path per version, so
    // preparation stays sequential even though supervision runs in parallel.
    let mut items = Vec::with_capacity(set.len());
    for (label, &index) in labels.iter().zip(&set) {
        let candidate = &workspace.candidates[index];
        let path_prepend = prepare_runtimes(candidate, &workspace.root, cli.quiet)?;
        items.push(LaunchItem {
            label: label.clone(),
            candidate,
            path_prepend,
            env: prepare_env(candidate),
        });
    }
    supervise::run_many(&items, options)
}

/// One line when a root orchestrator hides visible sub-apps, so the default
/// set never looks like it silently dropped work.
fn note_orchestration(candidates: &[Candidate], plan: &launch::LaunchPlan) {
    let Some(&first) = plan.set.first() else {
        return;
    };
    let Some(&suppressed) = plan.suppressed.first() else {
        return;
    };
    println!(
        "  note       orchestrated by {}; --select {} for one app",
        candidates[first].id(),
        candidates[suppressed].id()
    );
}

fn labels_for(set: &[usize], candidates: &[Candidate]) -> Vec<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for &index in set {
        *counts
            .entry(candidates[index].spec.name.clone())
            .or_insert(0) += 1;
    }

    let mut seen: HashMap<String, usize> = HashMap::new();
    set.iter()
        .map(|&index| {
            let candidate = &candidates[index];
            if counts.get(&candidate.spec.name) == Some(&1) {
                return candidate.spec.name.clone();
            }
            if !candidate.rel.as_os_str().is_empty() {
                return candidate.id();
            }
            let count = seen.entry(candidate.spec.name.clone()).or_insert(0);
            *count += 1;
            format!("{}#{count}", candidate.spec.name)
        })
        .collect()
}

fn print_intro(workspace_root: &Path, candidate: &Candidate) {
    println!("  srvm {}", env!("CARGO_PKG_VERSION"));
    println!("  workspace  {}", workspace_root.display());
    if candidate.rel.as_os_str().is_empty() {
        println!("  serve      {}", candidate.spec.summary());
    } else {
        println!(
            "  serve      [{}] {}",
            candidate.rel.display(),
            candidate.spec.summary()
        );
    }
}

fn print_dry_run(
    workspace: &workspace::Workspace,
    port: Option<u16>,
    select: Option<&str>,
) -> Result<()> {
    println!("  srvm {}", env!("CARGO_PKG_VERSION"));
    println!("  workspace  {}", workspace.root.display());

    if workspace.candidates.is_empty() {
        println!("  detect     no servable app detected");
        println!(
            "  looked     {}, {}/{{{}}}, {{{}}}",
            workspace::APP_DIRS.join(", "),
            workspace::APP_PARENTS.join(", "),
            workspace::APP_DIRS.join(","),
            workspace::ASSET_DIRS.join(",")
        );
        return Ok(());
    }

    for (index, candidate) in workspace.candidates.iter().enumerate() {
        println!("  match      {}. {}", index + 1, candidate.spec.name);
        println!("  root       {}", display_rel(candidate));
        println!("  command    {}", candidate.spec.command_line());
        if let Some(install) = &candidate.spec.install {
            println!("  install    {}", install.command_line());
        }
        let env_file = dotenv::load(&candidate.root);
        if !env_file.pairs.is_empty() {
            println!("  env        .env ({} vars)", env_file.pairs.len());
        }
        for note in env_file.notes {
            eprintln!("  warn       [{}] {note}", display_rel(candidate));
        }
        if let Some(sample) = dotenv::sample_without_env(&candidate.root) {
            println!(
                "  note       [{}] no .env; {sample} exists — copy it if the app needs configuration",
                display_rel(candidate)
            );
        }
        print_runtime_note(&workspace.root, candidate);

        let inherited = match &candidate.spec.port {
            PortInjection::Env(key) => env::var(key).ok(),
            _ => None,
        };
        match ports::requested_port(&candidate.spec, port, inherited.as_deref())? {
            Some(start) => {
                if start == 0 {
                    println!("  port       0 (OS-assigned free port chosen at launch)");
                } else {
                    println!("  port       {start} (start; availability checked at launch)");
                }
                println!("  override   {}", override_description(&candidate.spec));
            }
            None => {
                if let Some(hint) = candidate.spec.url_hint {
                    println!("  port       {hint}");
                }
                if port.is_some() && matches!(candidate.spec.port, PortInjection::None) {
                    println!("  override   {}", override_description(&candidate.spec));
                }
            }
        }
    }

    // `--select` is what would run, so the report must not pretend otherwise.
    if let Some(select) = select {
        let index = select_index(&workspace.candidates, select)?;
        println!(
            "  launch     1. {}  [{}]",
            workspace.candidates[index].id(),
            display_rel(&workspace.candidates[index])
        );
        return Ok(());
    }

    let plan = launch::plan_default(&workspace.candidates);
    for (position, &index) in plan.set.iter().enumerate() {
        println!(
            "  launch     {}. {}  [{}]",
            position + 1,
            workspace.candidates[index].id(),
            display_rel(&workspace.candidates[index])
        );
    }
    if plan.orchestrated {
        note_orchestration(&workspace.candidates, &plan);
    }
    if !plan.suppressed.is_empty() {
        let ids = plan
            .suppressed
            .iter()
            .map(|&index| {
                format!(
                    "{} ({})",
                    workspace.candidates[index].id(),
                    display_rel(&workspace.candidates[index])
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!("  idle       {ids}");
    }
    Ok(())
}

fn display_rel(candidate: &Candidate) -> String {
    if candidate.rel.as_os_str().is_empty() {
        ".".into()
    } else {
        candidate.rel.to_string_lossy().into_owned()
    }
}

fn print_runtime_note(workspace_root: &Path, candidate: &Candidate) {
    let mut seen = Vec::new();
    for program in programs_of(&candidate.spec) {
        if program_file(&candidate.root, program).is_some() {
            continue;
        }
        let tool = bare_tool(program);
        if binpath::look_path_within(&tool, &candidate.root, Some(workspace_root)).is_some() {
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

/// The app's own `.env`, injected for keys the ambient environment does not
/// already define. Parsing problems are reported, never fatal: a repository
/// with a questionable `.env` still runs.
fn prepare_env(candidate: &Candidate) -> Vec<(String, String)> {
    let (pairs, notes) = dotenv::child_env(&candidate.root);
    for note in notes {
        eprintln!("  warn       [{}] {note}", display_rel(candidate));
    }
    if let Some(sample) = dotenv::sample_without_env(&candidate.root) {
        println!(
            "  note       [{}] no .env; {sample} exists — copy it if the app needs configuration",
            display_rel(candidate)
        );
    }
    pairs
}

fn prepare_runtimes(
    candidate: &Candidate,
    workspace_root: &Path,
    quiet: bool,
) -> Result<Vec<PathBuf>> {
    let root = &candidate.root;
    let mut dirs = Vec::new();
    let mut seen = Vec::new();
    for program in programs_of(&candidate.spec) {
        if let Some(file) = program_file(root, program) {
            if let Some(dir) = file.parent() {
                push_unique(&mut dirs, dir.to_path_buf());
            }
            continue;
        }
        let tool = bare_tool(program);
        if let Some(resolved) = binpath::look_path_within(&tool, root, Some(workspace_root)) {
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
        let scope = Scope::within(root, workspace_root);
        dirs.extend(runtime::fetch_if_missing(kind, &scope, quiet)?);
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

/// Resolves `--select` against every discovered candidate: a 1-based dry-run
/// index, an exact qualified id (`apps/web:package:dev`), or a bare name, tool,
/// or relative directory when exactly one candidate matches it.
fn select_index(candidates: &[Candidate], select: &str) -> Result<usize> {
    if let Ok(index) = select.parse::<usize>() {
        if (1..=candidates.len()).contains(&index) {
            return Ok(index - 1);
        }
        bail!(
            "--select {index} is out of range; this workspace has {} candidate(s)",
            candidates.len()
        );
    }

    if let Some(index) = candidates.iter().position(|c| c.id() == select) {
        return Ok(index);
    }

    let matches: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            candidate.spec.name == select
                || candidate.spec.tool == select
                || candidate.rel.to_string_lossy() == select
        })
        .map(|(index, _)| index)
        .collect();

    match matches.len() {
        0 => bail!("no detected app matches --select {select:?}"),
        1 => Ok(matches[0]),
        _ => bail!(
            "--select {select:?} matches {} apps: {}; use one of those qualified ids",
            matches.len(),
            matches
                .iter()
                .map(|&index| candidates[index].id())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::CommandSpec;
    use std::fs;

    fn candidate(root: &Path, rel: &str, program: &str) -> Candidate {
        Candidate {
            root: root.to_path_buf(),
            rel: PathBuf::from(rel),
            spec: ServeSpec::new(
                "test",
                "test",
                CommandSpec::new(program, Vec::<String>::new()),
                None,
                PortInjection::None,
            ),
        }
    }

    #[test]
    fn repo_local_program_prepends_its_own_bin_dir() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join(".venv").join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("python"), "#!/bin/sh\n").unwrap();
        let candidate = candidate(root.path(), "", ".venv/bin/python");

        let dirs = prepare_runtimes(&candidate, root.path(), true).unwrap();

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

    #[test]
    fn select_resolves_ids_names_dirs_and_rejects_ambiguity() {
        let root = tempfile::tempdir().unwrap();
        let mut candidates = vec![
            candidate(root.path(), "", "npm"),
            candidate(&root.path().join("frontend"), "frontend", "npm"),
            candidate(&root.path().join("backend"), "backend", "python3"),
        ];

        assert_eq!(select_index(&candidates, "2").unwrap(), 1);
        assert_eq!(select_index(&candidates, "backend").unwrap(), 2);

        candidates[0].spec.name = "package:dev".into();
        candidates[1].spec.name = "package:dev".into();
        candidates[0].spec.tool = "npm".into();
        candidates[1].spec.tool = "npm".into();
        assert_eq!(
            select_index(&candidates, "package:dev").unwrap(),
            0,
            "an exact qualified id wins even when another app shares the name"
        );

        let err = select_index(&candidates, "npm").unwrap_err();
        assert!(err.to_string().contains("frontend:package:dev"), "{err}");
        assert_eq!(
            select_index(&candidates, "frontend:package:dev").unwrap(),
            1
        );
        assert_eq!(select_index(&candidates, "frontend").unwrap(), 1);
        assert!(select_index(&candidates, "9").is_err());
        assert!(select_index(&candidates, "nope").is_err());
    }

    #[test]
    fn labels_qualify_only_when_needed() {
        let root = tempfile::tempdir().unwrap();
        let candidates = vec![
            candidate(&root.path().join("apps/web"), "apps/web", "npm"),
            candidate(&root.path().join("apps/admin"), "apps/admin", "npm"),
        ];

        assert_eq!(
            labels_for(&[0, 1], &candidates),
            vec!["apps/web:test", "apps/admin:test"]
        );
        assert_eq!(labels_for(&[0], &candidates), vec!["test"]);
    }
}
