use std::{collections::BTreeSet, path::Path};

use anyhow::Result;
use regex::Regex;

use super::{PortInjection, ServeSpec, ToolResolver, command_if_resolved, probe};

const TARGET_ORDER: &[&str] = &["dev", "serve", "server", "run", "start"];

pub fn rule_procfile(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    if !probe::file_exists(root, "Procfile") {
        return Ok(None);
    }

    for tool in ["foreman", "overmind", "hivemind"] {
        if let Some(command) = command_if_resolved(resolver, root, tool, ["start"]) {
            return Ok(Some(ServeSpec::new(
                "procfile",
                tool,
                command,
                None,
                PortInjection::Env("PORT".into()),
            )));
        }
    }

    Ok(None)
}

pub fn rule_make(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    for rel in ["Makefile", "GNUmakefile", "makefile"] {
        if probe::file_exists(root, rel) {
            return Ok(target_spec(root, resolver, rel, "make", "make"));
        }
    }
    Ok(None)
}

pub fn rule_just(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    for rel in ["justfile", "Justfile", ".justfile"] {
        if probe::file_exists(root, rel) {
            return Ok(target_spec(root, resolver, rel, "just", "just"));
        }
    }
    Ok(None)
}

pub fn rule_taskfile(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    for rel in ["Taskfile.yaml", "Taskfile.yml"] {
        if probe::file_exists(root, rel) {
            let text = probe::read_to_string(root, rel)?;
            for target in TARGET_ORDER {
                let pattern = format!(r"(?m)^\s{{2,}}{}:\s*$", regex::escape(target));
                if Regex::new(&pattern)?.is_match(&text) {
                    return Ok(command_if_resolved(resolver, root, "task", [*target]).map(
                        |command| {
                            ServeSpec::new(
                                format!("task:{target}"),
                                "task",
                                command,
                                None,
                                PortInjection::Env("PORT".into()),
                            )
                        },
                    ));
                }
            }
        }
    }
    Ok(None)
}

fn target_spec(
    root: &Path,
    resolver: &dyn ToolResolver,
    rel: &str,
    tool: &str,
    name: &str,
) -> Option<ServeSpec> {
    let targets = file_targets(root, rel).ok()?;
    let target = TARGET_ORDER
        .iter()
        .find(|target| targets.contains(**target))?;
    command_if_resolved(resolver, root, tool, [*target]).map(|command| {
        ServeSpec::new(
            format!("{name}:{target}"),
            tool,
            command,
            None,
            PortInjection::Env("PORT".into()),
        )
    })
}

fn file_targets(root: &Path, rel: &str) -> Result<BTreeSet<String>> {
    let text = probe::read_to_string(root, rel)?;
    let re = Regex::new(r"^([A-Za-z0-9_.-]+)\s*:(?:\s|$)")?;
    let mut targets = BTreeSet::new();

    for line in text.lines() {
        if line.starts_with(char::is_whitespace) || line.starts_with('#') || line.starts_with('.') {
            continue;
        }
        if line.contains(":=") || line.contains("?=") || line.contains("+=") || line.contains("%:")
        {
            continue;
        }
        if let Some(caps) = re.captures(line) {
            targets.insert(caps[1].to_string());
        }
    }

    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::tests::StubResolver;
    use tempfile::tempdir;

    #[test]
    fn parses_make_targets_without_variables_or_phony() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            ".PHONY: dev\nFOO := bar\n%: %.in\ndev:\n\t npm run dev\n",
        )
        .unwrap();

        let spec = rule_make(dir.path(), &StubResolver::with(&["make"]))
            .unwrap()
            .unwrap();

        assert_eq!(spec.command_line(), "make dev");
    }
}
