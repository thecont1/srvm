use std::path::{Path, PathBuf};

use anyhow::Result;

mod binpath;
mod js;
mod misc;
mod probe;
mod python;
mod targets;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    pub fn new(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    pub fn command_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortInjection {
    Env(String),
    Args(Vec<String>),
    Listener,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeSpec {
    pub name: String,
    pub tool: String,
    pub command: CommandSpec,
    pub install: Option<CommandSpec>,
    pub url_hint: Option<u16>,
    pub is_static: bool,
    pub port: PortInjection,
}

impl ServeSpec {
    pub fn new(
        name: impl Into<String>,
        tool: impl Into<String>,
        command: CommandSpec,
        url_hint: Option<u16>,
        port: PortInjection,
    ) -> Self {
        Self {
            name: name.into(),
            tool: tool.into(),
            command,
            install: None,
            url_hint,
            is_static: false,
            port,
        }
    }

    pub fn static_site(command: CommandSpec, url_hint: u16) -> Self {
        Self {
            name: "static".into(),
            tool: "srvm".into(),
            command,
            install: None,
            url_hint: Some(url_hint),
            is_static: true,
            port: PortInjection::Listener,
        }
    }

    pub fn command_line(&self) -> String {
        if self.is_static {
            "built-in static server".into()
        } else {
            self.command.command_line()
        }
    }

    pub fn summary(&self) -> String {
        format!("{} ({})", self.command_line(), self.tool)
    }
}

pub trait ToolResolver {
    fn resolve(&self, tool: &str, root: &Path) -> Option<PathBuf>;
}

#[derive(Debug, Default)]
pub struct PathResolver;

impl ToolResolver for PathResolver {
    fn resolve(&self, tool: &str, root: &Path) -> Option<PathBuf> {
        binpath::look_path(tool, root)
    }
}

pub struct AvailabilityResolver;

impl ToolResolver for AvailabilityResolver {
    fn resolve(&self, tool: &str, root: &Path) -> Option<PathBuf> {
        PathResolver
            .resolve(tool, root)
            .or_else(|| crate::runtime::can_fetch(tool).then(|| PathBuf::from(tool)))
    }
}

pub fn detect(root: &Path) -> Result<Vec<ServeSpec>> {
    detect_with(root, &AvailabilityResolver)
}

pub fn detect_with(root: &Path, resolver: &dyn ToolResolver) -> Result<Vec<ServeSpec>> {
    let mut specs = Vec::new();

    if let Some(spec) = js::rule_deno(root, resolver)? {
        specs.push(spec);
    }
    specs.extend(js::rule_package_json(root, resolver)?);
    if let Some(spec) = js::rule_wrangler(root, resolver)? {
        specs.push(spec);
    }
    if let Some(spec) = targets::rule_procfile(root, resolver)? {
        specs.push(spec);
    }
    if let Some(spec) = targets::rule_make(root, resolver)? {
        specs.push(spec);
    }
    if let Some(spec) = targets::rule_just(root, resolver)? {
        specs.push(spec);
    }
    if let Some(spec) = targets::rule_taskfile(root, resolver)? {
        specs.push(spec);
    }
    if let Some(spec) = python::rule_django(root, resolver)? {
        specs.push(spec);
    }
    if let Some(spec) = python::rule_uvicorn(root, resolver)? {
        specs.push(spec);
    }
    if let Some(spec) = python::rule_flask(root, resolver)? {
        specs.push(spec);
    }
    specs.extend(misc::rules(root, resolver)?);

    Ok(specs)
}

fn command_if_resolved(
    resolver: &dyn ToolResolver,
    root: &Path,
    tool: &str,
    args: impl IntoIterator<Item = impl Into<String>>,
) -> Option<CommandSpec> {
    resolver
        .resolve(tool, root)
        .map(|_| CommandSpec::new(tool, args))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{collections::HashSet, fs};
    use tempfile::{TempDir, tempdir};

    #[derive(Default)]
    pub struct StubResolver {
        tools: HashSet<String>,
    }

    impl StubResolver {
        pub fn with(tools: &[&str]) -> Self {
            Self {
                tools: tools.iter().map(|tool| tool.to_string()).collect(),
            }
        }
    }

    impl ToolResolver for StubResolver {
        fn resolve(&self, tool: &str, _root: &Path) -> Option<PathBuf> {
            self.tools.contains(tool).then(|| PathBuf::from(tool))
        }
    }

    #[test]
    fn detects_deno_and_wrangler_rules() {
        let deno = fixture(&[("deno.jsonc", r#"{"tasks":{"dev":"deno run main.ts"}}"#)]);
        assert_has(&detected(&deno, &["deno"]), "deno", "deno task dev");

        let wrangler = fixture(&[("wrangler.toml", "name = 'worker'\n")]);
        assert_has(
            &detected(&wrangler, &["wrangler"]),
            "wrangler",
            "wrangler dev",
        );
    }

    #[test]
    fn detects_package_json_workspace_and_framework_fallbacks() {
        let turbo = fixture(&[
            ("package.json", r#"{"scripts":{},"dependencies":{}}"#),
            ("turbo.json", "{}"),
        ]);
        assert_has(
            &detected(&turbo, &["npm", "npx"]),
            "turbo",
            "npx turbo run dev",
        );

        let nx = fixture(&[
            ("package.json", r#"{"scripts":{},"dependencies":{}}"#),
            ("nx.json", "{}"),
        ]);
        assert_has(
            &detected(&nx, &["npm", "npx"]),
            "nx",
            "npx nx run-many -t dev",
        );

        let next = fixture(&[(
            "package.json",
            r#"{"dependencies":{"next":"latest","vite":"latest"}}"#,
        )]);
        assert_has(&detected(&next, &["npm", "npx"]), "next", "npx next dev");
    }

    #[test]
    fn detects_procfile_make_just_and_taskfile_rules() {
        let procfile = fixture(&[("Procfile", "web: npm run dev\n")]);
        assert_has(
            &detected(&procfile, &["foreman"]),
            "procfile",
            "foreman start",
        );

        let make = fixture(&[("Makefile", "serve:\n\tpython -m http.server\n")]);
        assert_has(&detected(&make, &["make"]), "make:serve", "make serve");

        let just = fixture(&[("justfile", "server:\n  npm run dev\n")]);
        assert_has(&detected(&just, &["just"]), "just:server", "just server");

        let task = fixture(&[(
            "Taskfile.yml",
            "tasks:\n  start:\n    cmds:\n      - npm run dev\n",
        )]);
        assert_has(&detected(&task, &["task"]), "task:start", "task start");
    }

    #[test]
    fn detects_python_framework_rules() {
        let django = fixture(&[("manage.py", "")]);
        assert_has(
            &detected(&django, &["python3"]),
            "django",
            "python3 manage.py runserver",
        );

        let uvicorn = fixture(&[
            ("pyproject.toml", "fastapi = '*'\n"),
            ("main.py", "app = FastAPI()\n"),
        ]);
        assert_has(
            &detected(&uvicorn, &["uvicorn"]),
            "uvicorn",
            "uvicorn main:app --reload",
        );

        let flask = fixture(&[
            ("requirements.txt", "flask\n"),
            ("app.py", "app = Flask(__name__)\n"),
        ]);
        assert_has(
            &detected(&flask, &["flask"]),
            "flask",
            "flask --app app run",
        );
    }

    #[test]
    fn detects_ruby_rules() {
        let rails = fixture(&[("bin/rails", "#!/usr/bin/env ruby\n")]);
        assert_has(&detected(&rails, &[]), "rails", "bin/rails server");

        let jekyll = fixture(&[
            ("_config.yml", "title: docs\n"),
            ("Gemfile", "gem 'jekyll'\n"),
        ]);
        assert_has(
            &detected(&jekyll, &["bundle"]),
            "jekyll",
            "bundle exec jekyll serve",
        );

        let rackup = fixture(&[("config.ru", "run App\n"), ("Gemfile", "gem 'rack'\n")]);
        assert_has(
            &detected(&rackup, &["bundle"]),
            "rackup",
            "bundle exec rackup",
        );
    }

    #[test]
    fn detects_docs_elixir_and_php_rules() {
        let hugo = fixture(&[("hugo.toml", "baseURL = 'http://example.com'\n")]);
        assert_has(&detected(&hugo, &["hugo"]), "hugo", "hugo server");

        let mkdocs = fixture(&[("mkdocs.yml", "site_name: docs\n")]);
        assert_has(&detected(&mkdocs, &["mkdocs"]), "mkdocs", "mkdocs serve");

        let phoenix = fixture(&[("mix.exs", "{:phoenix, \"~> 1.7\"}\n")]);
        assert_has(&detected(&phoenix, &["mix"]), "phoenix", "mix phx.server");

        let laravel = fixture(&[("composer.json", "{}"), ("artisan", "#!/usr/bin/env php\n")]);
        assert_has(
            &detected(&laravel, &["php"]),
            "laravel",
            "php artisan serve",
        );
    }

    #[test]
    fn detects_rust_go_compose_and_static_rules() {
        let trunk = fixture(&[
            ("Cargo.toml", "[package]\nname='app'\n"),
            ("index.html", ""),
        ]);
        assert_has(
            &detected(&trunk, &["trunk", "cargo"]),
            "trunk",
            "trunk serve",
        );

        let cargo = fixture(&[
            ("Cargo.toml", "[package]\nname='app'\n"),
            ("src/main.rs", "fn main() {}\n"),
        ]);
        assert_has(&detected(&cargo, &["cargo"]), "cargo", "cargo run");

        let go_root = fixture(&[
            ("go.mod", "module example.com/app\n"),
            ("main.go", "package main\n"),
        ]);
        assert_has(&detected(&go_root, &["go"]), "go", "go run .");

        let compose = fixture(&[("compose.yaml", "services: {}\n")]);
        assert_has(
            &detected(&compose, &["docker"]),
            "compose",
            "docker compose up",
        );

        let static_site = fixture(&[("index.html", "hello\n")]);
        assert_has(
            &detected(&static_site, &[]),
            "static",
            "built-in static server",
        );
    }

    #[test]
    fn missing_binary_falls_through_to_later_rules() {
        let repo = fixture(&[
            ("deno.json", r#"{"tasks":{"dev":"deno run main.ts"}}"#),
            ("index.html", "hello\n"),
        ]);

        let specs = detected(&repo, &[]);

        assert!(!specs.iter().any(|spec| spec.name == "deno"));
        assert_has(&specs, "static", "built-in static server");
    }

    fn fixture(files: &[(&str, &str)]) -> TempDir {
        let dir = tempdir().unwrap();
        for (rel, content) in files {
            write_file(dir.path(), rel, content);
        }
        dir
    }

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn detected(root: &TempDir, tools: &[&str]) -> Vec<ServeSpec> {
        detect_with(root.path(), &StubResolver::with(tools)).unwrap()
    }

    fn assert_has(specs: &[ServeSpec], name: &str, command: &str) {
        assert!(
            specs
                .iter()
                .any(|spec| spec.name == name && spec.command_line() == command),
            "expected {name} -> {command}, got {specs:#?}"
        );
    }
}
