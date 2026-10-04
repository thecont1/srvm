use std::{collections::BTreeMap, path::Path};

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::{CommandSpec, PortInjection, ServeSpec, ToolResolver, command_if_resolved, probe};

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PackageJson {
    package_manager: Option<String>,
    scripts: Option<BTreeMap<String, String>>,
    dependencies: Option<BTreeMap<String, Value>>,
    dev_dependencies: Option<BTreeMap<String, Value>>,
}

pub fn rule_deno(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    let rel = if probe::file_exists(root, "deno.json") {
        "deno.json"
    } else if probe::file_exists(root, "deno.jsonc") {
        "deno.jsonc"
    } else {
        return Ok(None);
    };

    if resolver.resolve("deno", root).is_none() {
        return Ok(None);
    }

    let value = probe::read_jsonc(root, rel)?;
    let Some(tasks) = value.get("tasks").and_then(Value::as_object) else {
        return Ok(None);
    };

    for name in ["dev", "serve", "start"] {
        if tasks.contains_key(name) {
            return Ok(Some(ServeSpec::new(
                "deno",
                "deno",
                CommandSpec::new("deno", ["task", name]),
                None,
                PortInjection::Env("PORT".into()),
            )));
        }
    }

    Ok(None)
}

pub fn rule_package_json(root: &Path, resolver: &dyn ToolResolver) -> Result<Vec<ServeSpec>> {
    if !probe::file_exists(root, "package.json") {
        return Ok(Vec::new());
    }

    let pkg: PackageJson = serde_json::from_str(&probe::read_to_string(root, "package.json")?)?;
    let Some(pm) = pick_pm(root, resolver, pkg.package_manager.as_deref()) else {
        return Ok(Vec::new());
    };

    let mut specs = Vec::new();
    if let Some(script) = pkg.scripts.as_ref().and_then(pick_script) {
        let mut spec = ServeSpec::new(
            format!("package:{script}"),
            pm.name.clone(),
            CommandSpec::new(pm.name.clone(), ["run".to_string(), script]),
            None,
            PortInjection::Env("PORT".into()),
        );
        if !probe::dir_exists(root, "node_modules") {
            spec.install = Some(CommandSpec::new(pm.name.clone(), ["install"]));
        }
        specs.push(spec);
        return Ok(specs);
    }

    if probe::file_exists(root, "turbo.json")
        && let Some(exec) = pm.exec_command(root, resolver)
    {
        specs.push(ServeSpec::new(
            "turbo",
            pm.name.clone(),
            CommandSpec::new(exec, ["turbo", "run", "dev"]),
            None,
            PortInjection::Env("PORT".into()),
        ));
    }

    if probe::file_exists(root, "nx.json")
        && let Some(exec) = pm.exec_command(root, resolver)
    {
        specs.push(ServeSpec::new(
            "nx",
            pm.name.clone(),
            CommandSpec::new(exec, ["nx", "run-many", "-t", "dev"]),
            None,
            PortInjection::Env("PORT".into()),
        ));
    }

    let deps = collect_deps(&pkg);
    for fw in framework_bins() {
        if deps.contains(&fw.dep)
            && let Some(exec) = pm.exec_command(root, resolver)
        {
            let mut args = vec![fw.bin.to_string()];
            args.extend(fw.args.iter().map(|arg| arg.to_string()));
            specs.push(ServeSpec::new(
                fw.dep,
                pm.name.clone(),
                CommandSpec::new(exec, args),
                fw.port,
                fw.port_injection(),
            ));
            return Ok(specs);
        }
    }

    Ok(specs)
}

pub fn rule_wrangler(root: &Path, resolver: &dyn ToolResolver) -> Result<Option<ServeSpec>> {
    let has_marker = ["wrangler.toml", "wrangler.json", "wrangler.jsonc"]
        .iter()
        .any(|rel| probe::file_exists(root, rel));

    if !has_marker {
        return Ok(None);
    }

    Ok(
        command_if_resolved(resolver, root, "wrangler", ["dev"]).map(|command| {
            ServeSpec::new(
                "wrangler",
                "wrangler",
                command,
                Some(8787),
                PortInjection::Args(vec!["--port".into()]),
            )
        }),
    )
}

#[derive(Debug, Clone)]
struct PackageManager {
    name: String,
}

impl PackageManager {
    fn exec_command(&self, root: &Path, resolver: &dyn ToolResolver) -> Option<String> {
        let candidates: &[&str] = match self.name.as_str() {
            "npm" => &["npx"],
            "pnpm" => &["pnpm"],
            "yarn" => &["yarn"],
            "bun" => &["bun"],
            _ => &[],
        };
        candidates
            .iter()
            .find(|tool| resolver.resolve(tool, root).is_some())
            .map(|tool| (*tool).to_string())
    }
}

fn pick_pm(
    root: &Path,
    resolver: &dyn ToolResolver,
    field: Option<&str>,
) -> Option<PackageManager> {
    let mut candidates = Vec::new();

    if let Some(field) = field.and_then(|raw| raw.split('@').next()) {
        candidates.push(field.to_string());
    }

    if probe::file_exists(root, "bun.lock") || probe::file_exists(root, "bun.lockb") {
        candidates.push("bun".into());
    }
    if probe::file_exists(root, "pnpm-lock.yaml") || probe::file_exists(root, "pnpm-workspace.yaml")
    {
        candidates.push("pnpm".into());
    }
    if probe::file_exists(root, "yarn.lock") || probe::file_exists(root, ".yarnrc.yml") {
        candidates.push("yarn".into());
    }
    if probe::file_exists(root, "package-lock.json")
        || probe::file_exists(root, "npm-shrinkwrap.json")
    {
        candidates.push("npm".into());
    }
    candidates.extend(["npm".into(), "pnpm".into(), "yarn".into(), "bun".into()]);

    candidates.into_iter().find_map(|name| {
        resolver
            .resolve(&name, root)
            .map(|_| PackageManager { name })
    })
}

fn pick_script(scripts: &BTreeMap<String, String>) -> Option<String> {
    for exact in ["dev", "serve", "develop", "start", "preview"] {
        if scripts.contains_key(exact) {
            return Some(exact.to_string());
        }
    }

    scripts
        .keys()
        .filter(|name| {
            name.starts_with("dev:") || name.starts_with("dev_") || name.starts_with("dev-")
        })
        .min()
        .cloned()
}

fn collect_deps(pkg: &PackageJson) -> Vec<&str> {
    pkg.dependencies
        .iter()
        .chain(pkg.dev_dependencies.iter())
        .flat_map(|deps| deps.keys().map(String::as_str))
        .collect()
}

struct FrameworkBin {
    dep: &'static str,
    bin: &'static str,
    args: &'static [&'static str],
    port: Option<u16>,
    port_args: &'static [&'static str],
}

impl FrameworkBin {
    fn port_injection(&self) -> PortInjection {
        if self.port_args.is_empty() {
            PortInjection::Env("PORT".into())
        } else {
            PortInjection::Args(
                self.port_args
                    .iter()
                    .map(|arg| (*arg).to_string())
                    .collect(),
            )
        }
    }
}

fn framework_bins() -> &'static [FrameworkBin] {
    &[
        FrameworkBin {
            dep: "astro",
            bin: "astro",
            args: &["dev"],
            port: Some(4321),
            port_args: &["--port"],
        },
        FrameworkBin {
            dep: "next",
            bin: "next",
            args: &["dev"],
            port: Some(3000),
            port_args: &["--port"],
        },
        FrameworkBin {
            dep: "nuxt",
            bin: "nuxt",
            args: &["dev"],
            port: Some(3000),
            port_args: &["--port"],
        },
        FrameworkBin {
            dep: "@angular/core",
            bin: "ng",
            args: &["serve"],
            port: Some(4200),
            port_args: &["--port"],
        },
        FrameworkBin {
            dep: "@remix-run/dev",
            bin: "remix",
            args: &["dev"],
            port: Some(3000),
            port_args: &["--port"],
        },
        FrameworkBin {
            dep: "gatsby",
            bin: "gatsby",
            args: &["develop"],
            port: Some(8000),
            port_args: &["-p"],
        },
        FrameworkBin {
            dep: "@docusaurus/core",
            bin: "docusaurus",
            args: &["start"],
            port: Some(3000),
            port_args: &["--port"],
        },
        FrameworkBin {
            dep: "hexo",
            bin: "hexo",
            args: &["server"],
            port: Some(4000),
            port_args: &["--port"],
        },
        FrameworkBin {
            dep: "wrangler",
            bin: "wrangler",
            args: &["dev"],
            port: Some(8787),
            port_args: &["--port"],
        },
        FrameworkBin {
            dep: "vite",
            bin: "vite",
            args: &["dev"],
            port: Some(5173),
            port_args: &["--port"],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::tests::StubResolver;
    use tempfile::tempdir;

    #[test]
    fn picks_exact_script_before_framework() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"dev":"vite --host"},"dependencies":{"vite":"latest"}}"#,
        )
        .unwrap();

        let specs = rule_package_json(dir.path(), &StubResolver::with(&["npm"])).unwrap();

        assert_eq!(specs[0].name, "package:dev");
        assert_eq!(specs[0].command_line(), "npm run dev");
    }

    #[test]
    fn falls_back_to_framework_dep() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"vite":"latest"}}"#,
        )
        .unwrap();

        let specs = rule_package_json(dir.path(), &StubResolver::with(&["npm", "npx"])).unwrap();

        assert_eq!(specs[0].name, "vite");
        assert_eq!(specs[0].command_line(), "npx vite dev");
    }

    #[test]
    fn dev_variants_are_sorted() {
        let scripts = BTreeMap::from([("dev:z".into(), "z".into()), ("dev:a".into(), "a".into())]);

        assert_eq!(pick_script(&scripts), Some("dev:a".into()));
    }
}
