use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
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

    let value = probe::read_jsonc(root, rel)
        .with_context(|| format!("malformed {rel} in {}", root.display()))?;
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

    let pkg: PackageJson = serde_json::from_str(&probe::read_to_string(root, "package.json")?)
        .with_context(|| format!("malformed package.json in {}", root.display()))?;
    let Some(pm) = pick_pm(root, resolver, pkg.package_manager.as_deref()) else {
        return Ok(Vec::new());
    };

    let mut specs = Vec::new();
    if let Some(script) = pkg.scripts.as_ref().and_then(pick_script) {
        let framework = pkg
            .scripts
            .as_ref()
            .and_then(|scripts| scripts.get(&script))
            .and_then(|body| script_framework(body));
        let (hint, injection) = match framework {
            Some(fw) => (fw.port, script_injection(&pm, fw)),
            None => (None, PortInjection::Env("PORT".into())),
        };
        let mut spec = ServeSpec::new(
            format!("package:{script}"),
            pm.name.clone(),
            CommandSpec::new(pm.name.clone(), ["run".to_string(), script]),
            hint,
            injection,
        );
        if !probe::dir_exists(root, "node_modules") {
            spec.install = Some(CommandSpec::new(pm.name.clone(), ["install"]));
        }
        specs.push(spec);
        return Ok(specs);
    }

    if probe::file_exists(root, "turbo.json")
        && let Some(mut exec) = pm.exec_command(root, resolver)
    {
        exec.args
            .extend(["turbo", "run", "dev"].into_iter().map(String::from));
        specs.push(ServeSpec::new(
            "turbo",
            pm.name.clone(),
            exec,
            None,
            PortInjection::Env("PORT".into()),
        ));
    }

    if probe::file_exists(root, "nx.json")
        && let Some(mut exec) = pm.exec_command(root, resolver)
    {
        exec.args.extend(
            ["nx", "run-many", "-t", "dev"]
                .into_iter()
                .map(String::from),
        );
        specs.push(ServeSpec::new(
            "nx",
            pm.name.clone(),
            exec,
            None,
            PortInjection::Env("PORT".into()),
        ));
    }

    let deps = collect_deps(&pkg);
    for fw in framework_bins() {
        if deps.contains(&fw.dep)
            && let Some(mut exec) = pm.exec_command(root, resolver)
        {
            exec.args.push(fw.bin.to_string());
            exec.args.extend(fw.args.iter().map(|arg| arg.to_string()));
            specs.push(ServeSpec::new(
                fw.dep,
                pm.name.clone(),
                exec,
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
                PortInjection::Args(vec!["--port".into(), "{port}".into()]),
            )
        }),
    )
}

#[derive(Debug, Clone)]
struct PackageManager {
    name: String,
}

impl PackageManager {
    fn exec_command(&self, root: &Path, resolver: &dyn ToolResolver) -> Option<CommandSpec> {
        let (tool, wrapper): (&str, &[&str]) = match self.name.as_str() {
            "npm" => ("npx", &[]),
            "pnpm" => ("pnpm", &["exec"]),
            "yarn" => ("yarn", &[]),
            "bun" => ("bun", &["x"]),
            _ => return None,
        };
        resolver
            .resolve(tool, root)
            .map(|_| CommandSpec::new(tool, wrapper.iter().copied()))
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
            let mut args: Vec<String> = self
                .port_args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect();
            args.push("{port}".into());
            PortInjection::Args(args)
        }
    }
}

fn script_injection(pm: &PackageManager, fw: &FrameworkBin) -> PortInjection {
    match fw.port_injection() {
        PortInjection::Args(mut args) if pm.name == "npm" => {
            args.insert(0, "--".into());
            PortInjection::Args(args)
        }
        injection => injection,
    }
}

fn script_framework(body: &str) -> Option<&'static FrameworkBin> {
    let body = body.trim();
    if body.contains(['\n', '\r']) {
        return None;
    }
    let tokens: Vec<_> = body.split_whitespace().collect();
    framework_bins().iter().find(|fw| {
        (tokens.len() == 1 && tokens[0] == "vite" && fw.bin == "vite")
            || (tokens.first().copied() == Some(fw.bin) && tokens[1..] == *fw.args)
    })
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
            port_args: &[],
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

    #[test]
    fn recognizes_exact_framework_script_bodies() {
        for (body, expected) in [
            ("vite", "vite"),
            ("vite dev", "vite"),
            ("next dev", "next"),
            ("remix dev", "@remix-run/dev"),
            ("gatsby develop", "gatsby"),
            ("wrangler dev", "wrangler"),
        ] {
            assert_eq!(
                script_framework(body).map(|fw| fw.dep),
                Some(expected),
                "{body}"
            );
        }

        for body in [
            "",
            "vite --host",
            "next dev -p 5000",
            "node server.js",
            "echo ok && vite",
            "vite dev --port 3000",
            "VITE dev",
            "npx vite dev",
            "vite\ndev",
            "next\rdev",
        ] {
            assert!(script_framework(body).is_none(), "{body}");
        }
    }

    #[test]
    fn recognized_script_gets_hint_and_args_forwarding() {
        let spec = script_spec(r#"{"scripts":{"dev":"vite"}}"#, &["npm", "npx"]);
        assert_eq!(spec.url_hint, Some(5173));
        assert_eq!(
            spec.port,
            PortInjection::Args(vec!["--".into(), "--port".into(), "{port}".into()])
        );

        for pm in ["pnpm", "yarn", "bun"] {
            let spec = script_spec(r#"{"scripts":{"dev":"vite"}}"#, &[pm]);
            assert_eq!(
                spec.port,
                PortInjection::Args(vec!["--port".into(), "{port}".into()]),
                "{pm} must not add a -- separator"
            );
        }
    }

    #[test]
    fn recognized_remix_script_uses_env_not_hmr_port() {
        let spec = script_spec(
            r#"{"scripts":{"dev":"remix dev"},"dependencies":{"@remix-run/dev":"latest"}}"#,
            &["npm", "npx"],
        );
        assert_eq!(spec.url_hint, Some(3000));
        assert_eq!(spec.port, PortInjection::Env("PORT".into()));
    }

    #[test]
    fn opaque_script_stays_env_only() {
        let spec = script_spec(r#"{"scripts":{"dev":"vite --host"}}"#, &["npm", "npx"]);
        assert_eq!(spec.url_hint, None);
        assert_eq!(spec.port, PortInjection::Env("PORT".into()));
    }

    #[test]
    fn pm_exec_wrappers_prefix_framework_and_monorepo_commands() {
        let exec_line = |pkg: &str, extra: &str, tools: &[&str]| {
            let dir = tempdir().unwrap();
            std::fs::write(dir.path().join("package.json"), pkg).unwrap();
            if !extra.is_empty() {
                std::fs::write(dir.path().join(extra), "{}").unwrap();
            }
            rule_package_json(dir.path(), &StubResolver::with(tools))
                .unwrap()
                .first()
                .map(|spec| spec.command_line())
                .unwrap()
        };

        assert_eq!(
            exec_line(
                r#"{"scripts":{},"dependencies":{}}"#,
                "turbo.json",
                &["pnpm"]
            ),
            "pnpm exec turbo run dev"
        );
        assert_eq!(
            exec_line(
                r#"{"scripts":{},"dependencies":{}}"#,
                "turbo.json",
                &["bun"]
            ),
            "bun x turbo run dev"
        );
        assert_eq!(
            exec_line(r#"{"dependencies":{"vite":"latest"}}"#, "", &["pnpm"]),
            "pnpm exec vite dev"
        );
        assert_eq!(
            exec_line(r#"{"dependencies":{"vite":"latest"}}"#, "", &["bun"]),
            "bun x vite dev"
        );
        assert_eq!(
            exec_line(r#"{"dependencies":{"vite":"latest"}}"#, "", &["yarn"]),
            "yarn vite dev"
        );
    }

    fn script_spec(pkg: &str, tools: &[&str]) -> ServeSpec {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), pkg).unwrap();
        rule_package_json(dir.path(), &StubResolver::with(tools))
            .unwrap()
            .remove(0)
    }
}
