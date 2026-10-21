//! Per-user systemd integration. Secrets belong in a private EnvironmentFile.
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Subcommand)]
pub enum ServiceAction {
    /// Write a user service and enable it now. --no-start only writes the unit.
    Install {
        #[arg(long)]
        no_start: bool,
    },
    Start,
    Stop,
    Status,
    /// Disable and remove the unit; never remove the data directory.
    Uninstall,
}

pub(crate) fn quote(path: &Path) -> Result<String> {
    let text = path.to_str().context("systemd paths must be UTF-8")?;
    if text.chars().any(|c| c.is_control()) {
        bail!("control characters are not allowed in service paths");
    }
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    ))
}

pub fn unit(exe: &Path, data: &Path) -> Result<String> {
    Ok(format!(
        "[Unit]\nDescription=Drawer file storage\nAfter=network.target\n\n[Service]\nType=simple\nEnvironmentFile=-%h/.config/drawer/env\nExecStart={} --data-dir {} serve\nRestart=on-failure\nRestartSec=3\nUMask=0077\nNoNewPrivileges=yes\n\n[Install]\nWantedBy=default.target\n",
        quote(exe)?,
        quote(data)?
    ))
}

fn systemctl(args: &[&str]) -> Result<()> {
    if !Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl --user")?
        .success()
    {
        bail!(
            "systemctl --user {} failed; check your user systemd session",
            args.join(" ")
        );
    }
    Ok(())
}

pub fn run(action: &ServiceAction, data: &Path) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("user service management requires Linux with systemd");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .context("HOME must name an absolute directory")?;
    let dir = home.join(".config/systemd/user");
    let path = dir.join("drawer.service");
    match action {
        ServiceAction::Install { no_start } => {
            fs::create_dir_all(&dir)?;
            let content = unit(&std::env::current_exe()?, &std::path::absolute(data)?)?;
            // Do not overwrite an operator's existing custom unit.
            use std::io::Write;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .with_context(|| format!("{} already exists or cannot be created", path.display()))?
                .write_all(content.as_bytes())?;
            println!("Installed {}", path.display());
            if !no_start {
                systemctl(&["daemon-reload"])?;
                systemctl(&["enable", "--now", "drawer.service"])?;
            }
        }
        ServiceAction::Uninstall => {
            systemctl(&["disable", "--now", "drawer.service"])?;
            fs::remove_file(&path)?;
            systemctl(&["daemon-reload"])?;
        }
        ServiceAction::Start => systemctl(&["start", "drawer.service"])?,
        ServiceAction::Stop => systemctl(&["stop", "drawer.service"])?,
        ServiceAction::Status => systemctl(&["status", "--no-pager", "drawer.service"])?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_escapes_paths_and_uses_private_secrets_file() {
        let text = unit(
            Path::new("/home/a b/100%/drawer"),
            Path::new("/home/a b/.drawer"),
        )
        .unwrap();
        assert!(text.contains("100%%/drawer\" --data-dir \"/home/a b/.drawer\" serve"));
        assert!(text.contains("EnvironmentFile=-%h/.config/drawer/env"));
        assert!(text.contains("UMask=0077"));
        assert!(unit(Path::new("/bad\npath"), Path::new("/data")).is_err());
    }
}
