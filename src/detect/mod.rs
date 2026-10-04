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
            port: PortInjection::None,
        }
    }

    pub fn command_line(&self) -> String {
        self.command.command_line()
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

pub fn detect(root: &Path) -> Result<Vec<ServeSpec>> {
    detect_with(root, &PathResolver)
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
    use std::collections::HashSet;

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
}
