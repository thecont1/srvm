//! Dev-only generator: emits shell completions and the man page from the one
//! CLI schema (`srvm::command()`), so generated assets cannot drift from what
//! the parser actually accepts.
//!
//! Usage: `cargo run --example generate_cli_assets -- <assets-dir>`

use std::{fs, path::Path};

use clap_complete::{Shell, generate_to};

fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new("assets").to_path_buf());
    let completions = out.join("completions");
    fs::create_dir_all(&completions)?;

    let mut cmd = srvm::command();
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell] {
        generate_to(shell, &mut cmd, "srvm", &completions)?;
    }

    let man_dir = out.join("man");
    fs::create_dir_all(&man_dir)?;
    let mut buffer = Vec::new();
    clap_mangen::Man::new(srvm::command()).render(&mut buffer)?;
    // clap_mangen emits trailing spaces; committed output must pass `git diff --check`.
    let man = String::from_utf8(buffer)?;
    let man = man.lines().map(str::trim_end).collect::<Vec<_>>().join("\n") + "\n";
    fs::write(man_dir.join("srvm.1"), man)?;

    println!("assets written under {}", out.display());
    Ok(())
}
