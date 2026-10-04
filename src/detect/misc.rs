use std::{fs, path::Path};

use anyhow::Result;

use super::{CommandSpec, PortInjection, ServeSpec, ToolResolver, command_if_resolved, probe};

pub fn rules(root: &Path, resolver: &dyn ToolResolver) -> Result<Vec<ServeSpec>> {
    let mut specs = Vec::new();

    if let Some(spec) = rule_rails(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_jekyll(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_rackup(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_hugo(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_mkdocs(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_phoenix(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_laravel(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_trunk(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_cargo(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_go(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_compose(root, resolver) {
        specs.push(spec);
    }
    if let Some(spec) = rule_static(root) {
        specs.push(spec);
    }

    Ok(specs)
}

fn rule_rails(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    if probe::file_exists(root, "bin/rails") {
        return Some(ServeSpec::new(
            "rails",
            "rails",
            CommandSpec::new("bin/rails", ["server"]),
            Some(3000),
            PortInjection::Args(vec!["-p".into()]),
        ));
    }

    if probe::file_exists(root, "config/application.rb")
        && resolver.resolve("bundle", root).is_some()
    {
        return Some(ServeSpec::new(
            "rails",
            "bundle",
            CommandSpec::new("bundle", ["exec", "rails", "server"]),
            Some(3000),
            PortInjection::Args(vec!["-p".into()]),
        ));
    }

    None
}

fn rule_jekyll(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    if probe::file_exists(root, "_config.yml")
        && probe::file_contains(root, "Gemfile", "jekyll")
        && resolver.resolve("bundle", root).is_some()
    {
        return Some(ServeSpec::new(
            "jekyll",
            "bundle",
            CommandSpec::new("bundle", ["exec", "jekyll", "serve"]),
            Some(4000),
            PortInjection::Args(vec!["-P".into()]),
        ));
    }
    None
}

fn rule_rackup(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    if probe::file_exists(root, "config.ru")
        && probe::file_contains(root, "Gemfile", "rack")
        && resolver.resolve("bundle", root).is_some()
    {
        return Some(ServeSpec::new(
            "rackup",
            "bundle",
            CommandSpec::new("bundle", ["exec", "rackup"]),
            Some(9292),
            PortInjection::Args(vec!["-p".into()]),
        ));
    }
    None
}

fn rule_hugo(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    let has_marker = ["hugo.toml", "hugo.yaml", "hugo.yml", "hugo.json"]
        .iter()
        .any(|rel| probe::file_exists(root, rel))
        || ["config.toml", "config.yaml", "config.yml", "config.json"]
            .iter()
            .any(|rel| probe::file_contains(root, rel, "baseURL"));

    if has_marker {
        return command_if_resolved(resolver, root, "hugo", ["server"]).map(|command| {
            ServeSpec::new(
                "hugo",
                "hugo",
                command,
                Some(1313),
                PortInjection::Args(vec!["--port".into()]),
            )
        });
    }
    None
}

fn rule_mkdocs(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    if probe::file_exists(root, "mkdocs.yml") || probe::file_exists(root, "mkdocs.yaml") {
        return command_if_resolved(resolver, root, "mkdocs", ["serve"]).map(|command| {
            ServeSpec::new(
                "mkdocs",
                "mkdocs",
                command,
                Some(8000),
                PortInjection::Args(vec!["-a".into(), "127.0.0.1".into()]),
            )
        });
    }
    None
}

fn rule_phoenix(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    if probe::file_contains(root, "mix.exs", "phoenix") {
        return command_if_resolved(resolver, root, "mix", ["phx.server"]).map(|command| {
            ServeSpec::new(
                "phoenix",
                "mix",
                command,
                Some(4000),
                PortInjection::Env("PORT".into()),
            )
        });
    }
    None
}

fn rule_laravel(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    if probe::file_exists(root, "composer.json") && probe::file_exists(root, "artisan") {
        return command_if_resolved(resolver, root, "php", ["artisan", "serve"]).map(|command| {
            ServeSpec::new(
                "laravel",
                "php",
                command,
                Some(8000),
                PortInjection::Args(vec!["--port".into()]),
            )
        });
    }
    None
}

fn rule_trunk(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    if probe::file_exists(root, "Cargo.toml") && probe::file_exists(root, "index.html") {
        return command_if_resolved(resolver, root, "trunk", ["serve"]).map(|command| {
            ServeSpec::new(
                "trunk",
                "trunk",
                command,
                Some(8080),
                PortInjection::Args(vec!["--port".into()]),
            )
        });
    }
    None
}

fn rule_cargo(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    if probe::file_exists(root, "Cargo.toml") && probe::file_exists(root, "src/main.rs") {
        return command_if_resolved(resolver, root, "cargo", ["run"]).map(|command| {
            ServeSpec::new(
                "cargo",
                "cargo",
                command,
                None,
                PortInjection::Env("PORT".into()),
            )
        });
    }
    None
}

fn rule_go(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    if !probe::file_exists(root, "go.mod") || resolver.resolve("go", root).is_none() {
        return None;
    }

    if probe::file_exists(root, "main.go") {
        return Some(ServeSpec::new(
            "go",
            "go",
            CommandSpec::new("go", ["run", "."]),
            None,
            PortInjection::Env("PORT".into()),
        ));
    }

    let cmd_dir = root.join("cmd");
    let mut mains = Vec::new();
    if let Ok(entries) = fs::read_dir(cmd_dir) {
        for entry in entries.flatten() {
            if entry.path().join("main.go").is_file()
                && let Some(name) = entry.file_name().to_str()
            {
                mains.push(name.to_string());
            }
        }
    }

    if mains.len() == 1 {
        return Some(ServeSpec::new(
            format!("go:{}", mains[0]),
            "go",
            CommandSpec::new("go", ["run".to_string(), format!("./cmd/{}", mains[0])]),
            None,
            PortInjection::Env("PORT".into()),
        ));
    }

    None
}

fn rule_compose(root: &Path, resolver: &dyn ToolResolver) -> Option<ServeSpec> {
    let has_marker = [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yaml",
        "docker-compose.yml",
    ]
    .iter()
    .any(|rel| probe::file_exists(root, rel));

    if has_marker && resolver.resolve("docker", root).is_some() {
        return Some(ServeSpec::new(
            "compose",
            "docker",
            CommandSpec::new("docker", ["compose", "up"]),
            None,
            PortInjection::None,
        ));
    }
    None
}

fn rule_static(root: &Path) -> Option<ServeSpec> {
    probe::file_exists(root, "index.html")
        .then(|| ServeSpec::static_site(CommandSpec::new("srvm", ["static", "."]), 8000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::tests::StubResolver;
    use tempfile::tempdir;

    #[test]
    fn detects_single_go_cmd() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("cmd/api")).unwrap();
        std::fs::write(dir.path().join("go.mod"), "module example.com/app\n").unwrap();
        std::fs::write(dir.path().join("cmd/api/main.go"), "package main\n").unwrap();

        let specs = rules(dir.path(), &StubResolver::with(&["go"])).unwrap();

        assert_eq!(specs[0].command_line(), "go run ./cmd/api");
    }

    #[test]
    fn detects_static_site() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "hello").unwrap();

        let specs = rules(dir.path(), &StubResolver::default()).unwrap();

        assert_eq!(specs[0].name, "static");
        assert!(specs[0].is_static);
    }
}
