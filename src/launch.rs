use std::collections::HashSet;

use crate::detect::ServeSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Js,
    Python,
    Ruby,
    Docs,
    Elixir,
    Php,
    Rust,
    Go,
    Orchestrator,
    Static,
}

pub fn family(spec: &ServeSpec) -> Family {
    if spec.is_static {
        return Family::Static;
    }
    let name = spec.name.split(':').next().unwrap_or(spec.name.as_str());
    match name {
        "procfile" | "make" | "just" | "task" | "turbo" | "nx" | "compose" => Family::Orchestrator,
        "django" | "uvicorn" | "flask" => Family::Python,
        "rails" | "jekyll" | "rackup" => Family::Ruby,
        "hugo" | "mkdocs" => Family::Docs,
        "phoenix" => Family::Elixir,
        "laravel" => Family::Php,
        "trunk" | "cargo" => Family::Rust,
        "go" => Family::Go,
        _ => tool_family(&spec.tool),
    }
}

fn tool_family(tool: &str) -> Family {
    match tool {
        "python" | "python3" | "uv" | "poetry" | "pipenv" => Family::Python,
        "bundle" | "rails" | "bin/rails" | "ruby" => Family::Ruby,
        "hugo" | "mkdocs" => Family::Docs,
        "mix" => Family::Elixir,
        "php" => Family::Php,
        "cargo" | "trunk" => Family::Rust,
        "go" => Family::Go,
        "make" | "just" | "task" | "foreman" | "overmind" | "hivemind" | "docker" => {
            Family::Orchestrator
        }
        _ => Family::Js,
    }
}

pub fn launch_set(specs: &[ServeSpec]) -> Vec<usize> {
    let mut apps = Vec::new();
    let mut seen = HashSet::new();
    let mut orchestrators = Vec::new();
    let mut statics = Vec::new();

    for (idx, spec) in specs.iter().enumerate() {
        match family(spec) {
            Family::Orchestrator => orchestrators.push(idx),
            Family::Static => statics.push(idx),
            fam => {
                if seen.insert(fam) {
                    apps.push(idx);
                }
            }
        }
    }

    if !apps.is_empty() {
        return apps;
    }
    if let Some(first) = orchestrators.first() {
        return vec![*first];
    }
    statics.into_iter().take(1).collect()
}

#[cfg(test)]
mod tests {
    use super::{Family, family, launch_set};
    use crate::detect::{CommandSpec, PortInjection, ServeSpec};

    fn spec(name: &str, tool: &str) -> ServeSpec {
        ServeSpec::new(
            name,
            tool,
            CommandSpec::new("x", ["y"]),
            None,
            PortInjection::None,
        )
    }

    fn static_spec() -> ServeSpec {
        ServeSpec::static_site(CommandSpec::new("srvm", ["static", "."]), 8000)
    }

    #[test]
    fn every_detection_name_classifies() {
        for (name, tool, expected) in [
            ("deno", "deno", Family::Js),
            ("package:dev", "npm", Family::Js),
            ("package:serve", "pnpm", Family::Js),
            ("next", "npm", Family::Js),
            ("vite", "yarn", Family::Js),
            ("astro", "bun", Family::Js),
            ("nuxt", "npm", Family::Js),
            ("@angular/core", "npm", Family::Js),
            ("@remix-run/dev", "npm", Family::Js),
            ("gatsby", "npm", Family::Js),
            ("@docusaurus/core", "npm", Family::Js),
            ("hexo", "npm", Family::Js),
            ("wrangler", "wrangler", Family::Js),
            ("wrangler", "npm", Family::Js),
            ("turbo", "npm", Family::Orchestrator),
            ("nx", "pnpm", Family::Orchestrator),
            ("procfile", "foreman", Family::Orchestrator),
            ("procfile", "overmind", Family::Orchestrator),
            ("procfile", "hivemind", Family::Orchestrator),
            ("make:dev", "make", Family::Orchestrator),
            ("just:serve", "just", Family::Orchestrator),
            ("task:start", "task", Family::Orchestrator),
            ("compose", "docker", Family::Orchestrator),
            ("django", "python", Family::Python),
            ("django", "python3", Family::Python),
            ("uvicorn", "uvicorn", Family::Python),
            ("uvicorn", "uv", Family::Python),
            ("flask", "flask", Family::Python),
            ("flask", "poetry", Family::Python),
            ("rails", "bin/rails", Family::Ruby),
            ("rails", "bundle", Family::Ruby),
            ("jekyll", "bundle", Family::Ruby),
            ("rackup", "bundle", Family::Ruby),
            ("hugo", "hugo", Family::Docs),
            ("mkdocs", "mkdocs", Family::Docs),
            ("phoenix", "mix", Family::Elixir),
            ("laravel", "php", Family::Php),
            ("trunk", "trunk", Family::Rust),
            ("cargo", "cargo", Family::Rust),
            ("go", "go", Family::Go),
            ("go:web", "go", Family::Go),
            ("go:cmd/api", "go", Family::Go),
        ] {
            assert_eq!(family(&spec(name, tool)), expected, "{name} via {tool}");
        }
        assert_eq!(family(&static_spec()), Family::Static);
    }

    #[test]
    fn keeps_first_candidate_per_family() {
        let specs = vec![
            spec("package:dev", "npm"),
            spec("vite", "npm"),
            spec("django", "python3"),
            spec("flask", "python3"),
            spec("rails", "bundle"),
        ];

        assert_eq!(launch_set(&specs), vec![0, 2, 4]);
    }

    #[test]
    fn orchestrators_and_static_drop_when_apps_exist() {
        let specs = vec![
            spec("package:dev", "npm"),
            spec("make:dev", "make"),
            spec("compose", "docker"),
            static_spec(),
        ];

        assert_eq!(launch_set(&specs), vec![0]);
    }

    #[test]
    fn orchestrator_alone_launches_first_only() {
        let specs = vec![spec("make:dev", "make"), spec("compose", "docker")];

        assert_eq!(launch_set(&specs), vec![0]);
    }

    #[test]
    fn static_alone_is_kept() {
        let specs = vec![static_spec()];
        assert_eq!(launch_set(&specs), vec![0]);

        let specs = vec![static_spec(), static_spec()];
        assert_eq!(launch_set(&specs), vec![0]);
    }
}
