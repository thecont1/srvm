use std::collections::HashSet;

use crate::{detect::ServeSpec, workspace::Candidate};

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

pub fn plan_default(candidates: &[Candidate]) -> LaunchPlan {
    // A recognized orchestrator at the workspace root supersedes the apps it
    // already starts; those stay visible in `--dry-run` and reachable through
    // `--select`.
    if let Some(index) = candidates.iter().position(|candidate| {
        candidate.rel.as_os_str().is_empty() && family(&candidate.spec) == Family::Orchestrator
    }) {
        return LaunchPlan {
            set: vec![index],
            orchestrated: true,
            suppressed: suppressed(candidates.len(), &[index]),
        };
    }

    let mut apps = Vec::new();
    let mut seen = HashSet::new();
    let mut orchestrators = Vec::new();
    let mut statics = Vec::new();

    for (index, candidate) in candidates.iter().enumerate() {
        match family(&candidate.spec) {
            Family::Orchestrator => orchestrators.push(index),
            Family::Static => statics.push(index),
            fam => {
                if seen.insert((candidate.root.clone(), fam)) {
                    apps.push(index);
                }
            }
        }
    }

    let (set, orchestrated) = if !apps.is_empty() {
        (apps, false)
    } else if let Some(&first) = orchestrators.first() {
        (vec![first], true)
    } else {
        (statics.into_iter().take(1).collect::<Vec<_>>(), false)
    };

    LaunchPlan {
        suppressed: suppressed(candidates.len(), &set),
        set,
        orchestrated,
    }
}

/// What bare `srvm` (and its `--all` alias) actually launches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// Indices into the workspace candidates, in launch order.
    pub set: Vec<usize>,
    /// The set is a single orchestrator that takes precedence over sub-apps.
    pub orchestrated: bool,
    /// Visible candidates the default set deliberately does not launch.
    pub suppressed: Vec<usize>,
}

fn suppressed(total: usize, set: &[usize]) -> Vec<usize> {
    (0..total).filter(|index| !set.contains(index)).collect()
}

#[cfg(test)]
mod tests {
    use super::{Family, family, plan_default};
    use crate::detect::{CommandSpec, PortInjection, ServeSpec};
    use crate::workspace::Candidate;

    fn candidate(rel: &str, name: &str, tool: &str) -> Candidate {
        let root = std::path::PathBuf::from("/ws");
        Candidate {
            root: if rel.is_empty() {
                root.clone()
            } else {
                root.join(rel)
            },
            rel: std::path::PathBuf::from(rel),
            spec: if name == "static" {
                static_spec()
            } else {
                spec(name, tool)
            },
        }
    }

    #[test]
    fn default_set_dedups_by_app_root_and_family() {
        let candidates = vec![
            candidate("", "package:dev", "npm"),
            candidate("", "vite", "npm"),
            candidate("frontend", "package:dev", "npm"),
            candidate("backend", "django", "python3"),
            candidate("backend", "flask", "flask"),
        ];

        let plan = plan_default(&candidates);

        assert_eq!(plan.set, vec![0, 2, 3]);
        assert_eq!(plan.suppressed, vec![1, 4]);
        assert!(!plan.orchestrated);
    }

    #[test]
    fn two_js_apps_in_different_roots_both_launch() {
        let candidates = vec![
            candidate("apps/web", "package:dev", "npm"),
            candidate("apps/admin", "package:dev", "npm"),
        ];

        assert_eq!(plan_default(&candidates).set, vec![0, 1]);
    }

    #[test]
    fn root_orchestrator_runs_alone() {
        let candidates = vec![
            candidate("", "make:dev", "make"),
            candidate("frontend", "package:dev", "npm"),
            candidate("backend", "django", "python3"),
        ];

        let plan = plan_default(&candidates);

        assert_eq!(plan.set, vec![0]);
        assert!(plan.orchestrated);
        assert_eq!(plan.suppressed, vec![1, 2]);
    }

    #[test]
    fn orchestrators_and_statics_drop_when_apps_exist() {
        let candidates = vec![
            candidate("frontend", "package:dev", "npm"),
            candidate("frontend", "make:dev", "make"),
            candidate("", "static", "srvm"),
        ];

        let plan = plan_default(&candidates);

        assert_eq!(plan.set, vec![0]);
        assert!(!plan.orchestrated);
    }

    #[test]
    fn sub_root_orchestrator_runs_alone_when_no_app_survives() {
        let candidates = vec![
            candidate("apps/web", "make:dev", "make"),
            candidate("apps/api", "compose", "docker"),
        ];

        let plan = plan_default(&candidates);

        assert_eq!(plan.set, vec![0]);
        assert!(plan.orchestrated);
    }

    #[test]
    fn static_runs_only_when_nothing_else_matches() {
        let candidates = vec![candidate("public", "static", "srvm")];
        assert_eq!(plan_default(&candidates).set, vec![0]);

        let candidates = vec![
            candidate("public", "static", "srvm"),
            candidate("site", "static", "srvm"),
        ];
        assert_eq!(plan_default(&candidates).set, vec![0]);

        assert!(plan_default(&[]).set.is_empty());
    }

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
}
