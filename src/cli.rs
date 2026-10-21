//! The `drawer` command line: the server commands, plus file operations on a store.
//!
//! File commands open the store directly, so no server has to be running. A store is a
//! directory holding `drawer.db` and `blobs/`. Writes are serialized inside one process only,
//! so don't run CLI writes against a store while a server is taking writes on it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::json;

use crate::error::Error;
use crate::service::{Drawer, Edit, Node, NodeUpdate, Properties, Ref, WriteOpts, WriteOutcome};

const DEFAULT_ADDR: &str = "127.0.0.1:8787";
const DB_FILE: &str = "drawer.db";

#[derive(Parser)]
#[command(
    name = "drawer",
    version,
    about = "Drawer: versioned filesystem storage for agents",
    long_about = "Drawer keeps files as versioned nodes in a store directory.\n\n\
        `drawer serve` runs the web UI, REST API and MCP endpoint. `drawer stdio` speaks MCP over \
        a pipe. The file commands (ls, put, cat, ...) work on the store directly, so they need no \
        server. Paths look like /notes/todo.md. Use `drawer mcp-config` to wire up Hermes or another client."
)]
pub struct Cli {
    /// Store directory, holding drawer.db and blobs/ (default: ~/.drawer).
    #[arg(long, env = "DRAWER_DATA_DIR", global = true, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Name recorded as the author of changes. Defaults to $USER.
    #[arg(long, env = "DRAWER_AUTHOR", global = true, value_name = "NAME")]
    author: Option<String>,
    /// Print JSON instead of text, for the commands that return data.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum MemoryAction {
    /// Save a note. Content comes from the argument, or from stdin when omitted.
    Remember {
        name: String,
        content: Option<String>,
        /// Tag to attach; repeat for several.
        #[arg(short, long)]
        tag: Vec<String>,
        /// Add to the end of an existing note instead of replacing it.
        #[arg(short, long)]
        append: bool,
    },
    /// Find notes by text and/or tag, newest first.
    Recall {
        query: Option<String>,
        #[arg(short, long)]
        tag: Option<String>,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// Print a note with its links and backlinks.
    Read { name: String },
    /// Delete a note (moves it to the trash; `drawer restore` brings it back).
    Forget { name: String },
    /// List tags with how many notes use each.
    Tags,
    /// Show how notes link to each other. Missing notes are marked with `?`.
    Graph,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Serve the web UI, REST API (/drawer/v1) and MCP (/mcp) over HTTP. The default.
    Serve(ServeArgs),
    /// Speak MCP over stdin and stdout, for clients that start drawer as a subprocess.
    Stdio,
    /// Install or manage the Linux user background service.
    Service {
        #[command(subcommand)]
        action: crate::background::ServiceAction,
    },
    /// Synchronize a local desktop folder with a remote Drawer workspace.
    Sync(crate::sync::SyncArgs),
    /// Preview or apply offline retention and orphan-blob cleanup.
    Cleanup(crate::cleanup::CleanupArgs),
    /// Create, verify, restore and schedule offline snapshots.
    Backup {
        #[command(subcommand)]
        action: crate::backup::BackupAction,
    },
    /// Register and manage read-only host folder mounts.
    Mount {
        #[command(subcommand)]
        action: crate::mounts::MountAction,
    },
    /// List a folder. Folders are shown with a trailing slash.
    Ls {
        #[arg(default_value = "/")]
        path: String,
        /// Show kind, revision, size and modified time.
        #[arg(short, long)]
        long: bool,
    },
    /// Show a folder and everything below it as a tree.
    Tree {
        #[arg(default_value = "/")]
        path: String,
        /// How many levels to show.
        #[arg(short = 'L', long, default_value_t = 32, value_name = "N")]
        depth: usize,
    },
    /// Create a folder and any missing parents.
    Mkdir { path: String },
    /// Upload a file as a new revision, creating it if needed. Use - to read stdin.
    Put(PutArgs),
    /// Print a file to stdout.
    Cat {
        path: String,
        /// Print this revision instead of the current one.
        #[arg(long, value_name = "N")]
        revision: Option<i64>,
    },
    /// Download a file. Writes to the file name in the current directory unless OUT is given.
    Get(GetArgs),
    /// Show a file or folder: its id, revision, size, type, grants and properties.
    Stat { path: String },
    /// Move a file or folder to the trash. Its id and history are kept.
    Rm { path: String },
    /// List the trash, with the ids that restore and purge take.
    Trash,
    /// Bring a trashed file or folder back, by id.
    Restore { id: String },
    /// Delete a trashed file or folder for good, by id. Needs --yes.
    Purge {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Rename or move a file or folder. Moving onto a folder puts it inside. Keeps the id.
    Mv { from: String, to: String },
    /// Copy a file to a new path. Shares the stored bytes, so nothing is uploaded again.
    Cp {
        from: String,
        to: String,
        /// Copy this revision instead of the current one.
        #[arg(long, value_name = "N")]
        revision: Option<i64>,
    },
    /// List a file's revisions, newest first.
    History { path: String },
    /// Show what changed in a text file between two revisions, as a unified diff.
    /// With no revisions, shows the latest change.
    Diff {
        path: String,
        /// Older revision. Defaults to the one before TO; 0 is an empty file.
        from: Option<i64>,
        /// Newer revision. Defaults to the current one.
        to: Option<i64>,
        /// Lines of context around each change.
        #[arg(short = 'U', long, default_value_t = 3, value_name = "N")]
        context: usize,
    },
    /// Replace text in a file, saved as one new revision. OLD must appear exactly once unless --all.
    Edit {
        path: String,
        /// Exact text to replace.
        #[arg(long)]
        old: String,
        /// Replacement text; empty deletes OLD.
        #[arg(long, default_value = "")]
        new: String,
        /// Replace every occurrence.
        #[arg(long)]
        all: bool,
        #[arg(short, long, default_value = "")]
        comment: String,
    },
    /// List the most recently changed files, newest first, with who changed them.
    Recent {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Make an older revision the current one. Adds a revision, so nothing is lost.
    Rollback {
        path: String,
        revision: i64,
        #[arg(short, long, default_value = "")]
        comment: String,
    },
    /// Search live file names and the text of file contents.
    Search {
        query: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// Read and write the AI memory vault (Markdown notes in /memory).
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Star a file or folder.
    Star { path: String },
    /// Remove a star.
    Unstar { path: String },
    /// List starred files and folders.
    Starred,
    /// List who has been granted access to a file or folder. No grants means open.
    Perms { path: String },
    /// Grant a principal a role (reader, writer or owner) on a node and everything below it.
    Share {
        path: String,
        principal: String,
        role: String,
    },
    /// Remove a principal's grant on a node.
    Unshare { path: String, principal: String },
    /// Check the store: database integrity, counts, blob files and the web UI.
    Doctor,
    /// Print the MCP config for an agent harness: stdio on this store, or --url for a running server.
    McpConfig {
        #[arg(long, value_enum, default_value_t = McpFormat::Hermes)]
        format: McpFormat,
        /// Connect to a running `drawer serve` at this MCP URL (e.g. http://127.0.0.1:8787/mcp)
        /// instead of starting drawer over stdio.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// Name the server entry. Tools show up as e.g. mcp__NAME__drawer_read.
        #[arg(long, default_value = "drawer")]
        name: String,
    },
    /// Update this install to the latest GitHub release: the binary and the web UI. Restart `serve` after.
    Update {
        /// Only report whether a newer release exists.
        #[arg(long)]
        check: bool,
    },
    /// Remove this install: the background service, the web UI and the binary. Your data is kept.
    Uninstall,
    /// Print a shell completion script, e.g. `drawer completions zsh > ~/.zfunc/_drawer`.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Args)]
pub struct ServeArgs {
    /// Address to listen on. Use 0.0.0.0:PORT to accept connections from other machines.
    #[arg(long, env = "DRAWER_ADDR", default_value = DEFAULT_ADDR)]
    addr: String,
    /// Require `Authorization: Bearer <token>` on every request. An empty value counts as unset.
    #[arg(long, env = "DRAWER_TOKEN")]
    token: Option<String>,
    /// Built UI directory. Defaults to ./ui/dist, then ../share/drawer/ui beside the binary.
    #[arg(long, env = "DRAWER_UI_DIR")]
    ui_dir: Option<PathBuf>,
}

impl ServeArgs {
    /// What `drawer` with no subcommand runs: serve, with the same settings as the flags.
    fn from_env() -> Self {
        Self {
            addr: std::env::var("DRAWER_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.into()),
            token: std::env::var("DRAWER_TOKEN").ok(),
            ui_dir: std::env::var_os("DRAWER_UI_DIR").map(PathBuf::from),
        }
    }
}

#[derive(Args)]
pub struct PutArgs {
    /// Local file to upload, or - for stdin.
    file: PathBuf,
    /// Destination in Drawer. A folder, or a path ending in /, takes the file name inside it.
    /// Defaults to the file name at the root, which stdin cannot use.
    dest: Option<String>,
    /// Write only while the file is at this revision. 0 means create only, never overwrite.
    #[arg(long, value_name = "N")]
    if_revision: Option<i64>,
    /// Note stored with the revision.
    #[arg(short, long, default_value = "")]
    comment: String,
    /// Property stored with the revision, as key=value. Repeatable.
    #[arg(long = "prop", value_name = "KEY=VALUE", value_parser = parse_prop)]
    props: Vec<(String, String)>,
}

#[derive(Args)]
pub struct GetArgs {
    /// Drawer path to download.
    path: String,
    /// Local file to write. Defaults to the file name. Use - for stdout.
    out: Option<PathBuf>,
    /// Download this revision instead of the current one.
    #[arg(long, value_name = "N")]
    revision: Option<i64>,
    /// Overwrite the local file if it exists.
    #[arg(long)]
    force: bool,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum McpFormat {
    /// YAML for ~/.hermes/config.yaml.
    Hermes,
    /// JSON with an mcpServers object, for Claude Desktop, Cursor, Windsurf and most other clients.
    Json,
    /// A `claude mcp add` command for Claude Code.
    ClaudeCode,
    /// TOML for ~/.codex/config.toml (OpenAI Codex CLI).
    Codex,
    /// JSON for ~/.gemini/settings.json (Gemini CLI uses httpUrl for streamable HTTP).
    Gemini,
    /// JSON for .vscode/mcp.json (VS Code / Copilot agent mode).
    Vscode,
    /// JSON for opencode.json (opencode).
    Opencode,
}

fn parse_prop(s: &str) -> std::result::Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got {s:?}"))
}

/// Entry point for the `drawer` binary. Prints errors to stderr and exits 1.
pub async fn run() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("drawer: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(cli: Cli) -> Result<()> {
    let cmd = cli.cmd.unwrap_or_else(|| Cmd::Serve(ServeArgs::from_env()));
    if let Cmd::Update { check } = &cmd {
        // Works on the install, not the store, so it runs before the store check below.
        return crate::update::run(*check);
    }
    if let Cmd::Uninstall = &cmd {
        let data_dir = cli.data_dir.clone().or_else(|| default_data_dir().ok());
        return crate::update::uninstall(data_dir.as_deref());
    }
    if let Cmd::Completions { shell } = &cmd {
        let mut command = <Cli as clap::CommandFactory>::command();
        clap_complete::generate(*shell, &mut command, "drawer", &mut std::io::stdout());
        return Ok(());
    }
    if let Cmd::Sync(args) = &cmd {
        return crate::sync::run(args).await;
    }
    // Resolve only for commands that use a store. Help, completions and self-update
    // must also work in minimal environments without a home directory.
    let data_dir = match cli.data_dir {
        Some(path) => path,
        None => default_data_dir()?,
    };
    if let Cmd::Service { action } = &cmd {
        return crate::background::run(action, &data_dir);
    }
    if let Cmd::Backup { action } = &cmd {
        return crate::backup::cli(action, &data_dir).await;
    }
    if let Cmd::Cleanup(args) = &cmd {
        println!(
            "{}",
            serde_json::to_string_pretty(&crate::cleanup::cleanup(&data_dir, args).await?)?
        );
        return Ok(());
    }
    if let Cmd::McpConfig { format, url, name } = &cmd {
        print!("{}", mcp_config(*format, &data_dir, url.as_deref(), name)?);
        return Ok(());
    }
    if matches!(cmd, Cmd::Serve(_) | Cmd::Stdio) {
        // Logs go to stderr so stdio MCP keeps stdout clean.
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .init();
    }
    // Only these start a store. Everything else needs one already there, so a typo in
    // --data-dir fails loudly instead of quietly creating an empty store.
    let creates = matches!(
        cmd,
        Cmd::Serve(_)
            | Cmd::Stdio
            | Cmd::Put(_)
            | Cmd::Mkdir { .. }
            | Cmd::Memory {
                action: MemoryAction::Remember { .. }
            }
    );
    if !creates && !data_dir.join(DB_FILE).exists() {
        bail!(
            "no store at {}; set --data-dir or DRAWER_DATA_DIR, or start one with `drawer serve` or `drawer put`",
            absolute(&data_dir).display()
        );
    }

    let svc = Drawer::open(&data_dir).await?;
    // MCP over stdio records the client's own name unless an author was set explicitly.
    let mcp_author = cli.author.clone();
    let actor = cli.author.unwrap_or_else(default_author);
    let json = cli.json;
    match cmd {
        Cmd::Mount { action } => crate::mounts::cli(&svc, action).await,
        Cmd::Serve(args) => serve(svc, &data_dir, args).await,
        Cmd::Stdio => {
            use rmcp::ServiceExt;
            let running = crate::mcp::DrawerMcp::new(svc)
                .with_author(mcp_author)
                .serve(rmcp::transport::stdio())
                .await?;
            running.waiting().await?;
            Ok(())
        }
        Cmd::Ls { path, long } => {
            let nodes = svc.list(&path).await?;
            if json {
                return emit_json(&nodes);
            }
            for n in &nodes {
                if long {
                    println!(
                        "{:<7} {:>4} {:>9}  {}  {}",
                        n.kind,
                        revision_label(n),
                        size_label(n),
                        short_time(&n.modified_at),
                        display_name(n)
                    );
                } else {
                    println!("{}", display_name(n));
                }
            }
            Ok(())
        }
        Cmd::Tree { path, depth } => {
            let (nodes, truncated) = svc.walk(&path, depth).await?;
            if json {
                return emit_json(&json!({ "entries": nodes, "truncated": truncated }));
            }
            println!("{}", crate::service::normalize(&path)?);
            print_tree(&nodes, &crate::service::normalize(&path)?);
            let files = nodes.iter().filter(|n| n.kind == "file").count();
            let count =
                |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
            let cut = if truncated { " (cut short)" } else { "" };
            println!(
                "\n{}, {}{cut}",
                count(nodes.len() - files, "folder"),
                count(files, "file")
            );
            Ok(())
        }
        Cmd::Mkdir { path } => {
            let node = svc.mkdir(&path, &actor).await?;
            if json {
                emit_json(&node)
            } else {
                println!("{}", node.path);
                Ok(())
            }
        }
        Cmd::Put(args) => {
            let (dest, data) = if args.file == Path::new("-") {
                // Stdin has no file name, so the destination must name the file itself.
                let dest = args
                    .dest
                    .clone()
                    .filter(|d| !d.ends_with('/'))
                    .ok_or_else(|| anyhow!("uploading from stdin needs a destination file path, like /notes/todo.md"))?;
                let mut buf = Vec::new();
                std::io::stdin()
                    .read_to_end(&mut buf)
                    .context("reading stdin")?;
                (dest, buf)
            } else {
                let name = args
                    .file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| anyhow!("{} has no usable file name", args.file.display()))?;
                let dest = target_path(&svc, args.dest.as_deref().unwrap_or("/"), name).await?;
                let data = tokio::fs::read(&args.file)
                    .await
                    .with_context(|| format!("reading {}", args.file.display()))?;
                (dest, data)
            };
            let opts = write_opts(&actor, &args.comment, args.props, args.if_revision);
            let out = svc.write(&dest, &data, opts).await?;
            report_write(&out, json)
        }
        Cmd::Cat { path, revision } => {
            let (_, data) = svc.read(Ref::Path(&path), revision).await?;
            std::io::stdout().write_all(&data)?;
            Ok(())
        }
        Cmd::Get(args) => {
            let (rev, data) = svc.read(Ref::Path(&args.path), args.revision).await?;
            match args.out.as_deref() {
                Some(out) if out == Path::new("-") => {
                    std::io::stdout().write_all(&data)?;
                }
                other => {
                    let out = other
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from(leaf(&args.path)));
                    if out.exists() && !args.force {
                        bail!(
                            "{} already exists; pass --force to overwrite it",
                            out.display()
                        );
                    }
                    tokio::fs::write(&out, &data)
                        .await
                        .with_context(|| format!("writing {}", out.display()))?;
                    println!(
                        "saved {} (revision {}, {}) to {}",
                        args.path,
                        rev.revision,
                        human_size(data.len() as i64),
                        out.display()
                    );
                }
            }
            Ok(())
        }
        Cmd::Stat { path } => {
            let node = svc.stat(Ref::Path(&path)).await?;
            if json {
                return emit_json(&node);
            }
            print_stat(&node);
            Ok(())
        }
        Cmd::Rm { path } => {
            let node = svc.trash(Ref::Path(&path), None, &actor).await?;
            if json {
                emit_json(&node)
            } else {
                println!("trashed {} (id {})", node.path, node.id);
                Ok(())
            }
        }
        Cmd::Trash => {
            let nodes = svc.trash_list().await?;
            if json {
                return emit_json(&nodes);
            }
            if nodes.is_empty() {
                println!("trash is empty");
            }
            for n in &nodes {
                println!("{}  {:<7} {}", n.id, n.kind, n.path);
            }
            Ok(())
        }
        Cmd::Restore { id } => {
            let node = svc
                .update(
                    &id,
                    NodeUpdate {
                        trashed: Some(false),
                        ..NodeUpdate::default()
                    },
                    &actor,
                )
                .await?;
            if json {
                emit_json(&node)
            } else {
                println!("restored {}", node.path);
                Ok(())
            }
        }
        Cmd::Purge { id, yes } => {
            if !yes {
                bail!("purge deletes {id} for good; pass --yes to confirm");
            }
            let node = svc.stat(Ref::Id(&id)).await?;
            svc.purge(&id, &actor).await?;
            println!("purged {} (id {})", node.path, id);
            Ok(())
        }
        Cmd::Mv { from, to } => {
            let dest = target_path(&svc, &to, leaf(&from)).await?;
            let node = svc.move_to(Ref::Path(&from), &dest, &actor).await?;
            if json {
                emit_json(&node)
            } else {
                println!("{from} -> {}", node.path);
                Ok(())
            }
        }
        Cmd::Cp { from, to, revision } => {
            let dest = target_path(&svc, &to, leaf(&from)).await?;
            let out = svc
                .copy(
                    Ref::Path(&from),
                    &dest,
                    revision,
                    write_opts(&actor, "", Vec::new(), None),
                )
                .await?;
            report_write(&out, json)
        }
        Cmd::History { path } => {
            let revs = svc.revisions(Ref::Path(&path)).await?;
            if json {
                return emit_json(&revs);
            }
            println!(
                "{:>4}  {:<16}  {:<12} {:>9}  COMMENT",
                "REV", "WHEN", "AUTHOR", "SIZE"
            );
            for r in &revs {
                println!(
                    "{:>4}  {:<16}  {:<12} {:>9}  {}",
                    r.revision,
                    short_time(&r.created_at),
                    r.author,
                    human_size(r.size),
                    r.comment
                );
            }
            Ok(())
        }
        Cmd::Diff {
            path,
            from,
            to,
            context,
        } => {
            let d = svc.diff(Ref::Path(&path), from, to, context).await?;
            if json {
                return emit_json(&d);
            }
            if d.diff.is_empty() {
                println!(
                    "{} revisions {} and {} have the same content",
                    d.path, d.from, d.to
                );
            } else {
                print_diff(&d.diff);
            }
            Ok(())
        }
        Cmd::Edit {
            path,
            old,
            new,
            all,
            comment,
        } => {
            let e = Edit {
                old,
                new,
                replace_all: all,
            };
            let out = svc
                .edit(&path, &[e], write_opts(&actor, &comment, Vec::new(), None))
                .await?;
            report_write(&out, json)
        }
        Cmd::Recent { limit } => {
            let nodes = svc.recent(limit).await?;
            if json {
                return emit_json(&nodes);
            }
            for n in &nodes {
                println!(
                    "{}  {:<12} {:>4}  {}",
                    short_time(&n.modified_at),
                    n.modified_by.as_deref().unwrap_or("-"),
                    revision_label(n),
                    n.path
                );
            }
            Ok(())
        }
        Cmd::Rollback {
            path,
            revision,
            comment,
        } => {
            let out = svc
                .rollback(
                    Ref::Path(&path),
                    revision,
                    write_opts(&actor, &comment, Vec::new(), None),
                )
                .await?;
            report_write(&out, json)
        }
        Cmd::Search { query, limit } => {
            let hits = svc.search(&query, limit).await?;
            if json {
                return emit_json(&hits);
            }
            if hits.is_empty() {
                println!("no matches for {query:?}");
            }
            for h in &hits {
                match h.matched {
                    "name" => println!("{}  (name)", h.path),
                    _ => println!("{}: {}", h.path, h.snippet),
                }
            }
            Ok(())
        }
        Cmd::Memory { action } => memory(&svc, action, &actor, json).await,
        Cmd::Star { path } => set_starred(&svc, &path, true, &actor, json).await,
        Cmd::Unstar { path } => set_starred(&svc, &path, false, &actor, json).await,
        Cmd::Starred => {
            let nodes = svc.starred().await?;
            if json {
                return emit_json(&nodes);
            }
            if nodes.is_empty() {
                println!("nothing starred");
            }
            for n in &nodes {
                println!("{}", n.path);
            }
            Ok(())
        }
        Cmd::Perms { path } => {
            let node = svc.stat(Ref::Path(&path)).await?;
            let grants = svc.permissions(&node.id).await?;
            if json {
                return emit_json(&grants);
            }
            if grants.is_empty() {
                println!("no grants on {path}: open to anyone with store access");
            }
            for g in &grants {
                println!("{:<24} {}", g.principal, g.role);
            }
            Ok(())
        }
        Cmd::Share {
            path,
            principal,
            role,
        } => {
            let node = svc.stat(Ref::Path(&path)).await?;
            let grants = svc.share(&node.id, &principal, &role, &actor).await?;
            if json {
                emit_json(&grants)
            } else {
                println!("{principal} is now {role} on {}", node.path);
                Ok(())
            }
        }
        Cmd::Unshare { path, principal } => {
            let node = svc.stat(Ref::Path(&path)).await?;
            let grants = svc.unshare(&node.id, &principal, &actor).await?;
            if json {
                emit_json(&grants)
            } else {
                println!("removed {principal} from {}", node.path);
                Ok(())
            }
        }
        Cmd::Doctor => doctor(&svc, &data_dir, json).await,
        Cmd::Sync(_)
        | Cmd::Cleanup(_)
        | Cmd::Backup { .. }
        | Cmd::Service { .. }
        | Cmd::McpConfig { .. }
        | Cmd::Completions { .. }
        | Cmd::Update { .. }
        | Cmd::Uninstall => {
            unreachable!("handled before the store is opened")
        }
    }
}

async fn serve(svc: Drawer, data_dir: &Path, args: ServeArgs) -> Result<()> {
    let token = args.token.filter(|t| !t.is_empty());
    let ui_dir = match args.ui_dir {
        Some(dir) => {
            if !dir.join("index.html").exists() {
                bail!(
                    "--ui-dir {} has no index.html; build the UI with `npm run build` in ui/",
                    dir.display()
                );
            }
            Some(dir)
        }
        None => default_ui_dir(),
    };
    let listener = tokio::net::TcpListener::bind(&args.addr)
        .await
        .with_context(|| format!("cannot listen on {}", args.addr))?;
    let bound = listener.local_addr()?;
    let url = format!("http://{bound}");
    eprintln!("drawer {} serving {url}", env!("CARGO_PKG_VERSION"));
    eprintln!("  store  {}", absolute(data_dir).display());
    eprintln!("  mcp    {url}/mcp");
    match &ui_dir {
        Some(dir) => eprintln!("  ui     {}", dir.display()),
        None => eprintln!("  ui     not built; `cd ui && npm run build` to add it"),
    }
    let password = std::env::var("DRAWER_ADMIN_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty());
    let secure_cookie = std::env::var("DRAWER_COOKIE_SECURE")
        .is_ok_and(|s| s == "1" || s.eq_ignore_ascii_case("true"));
    // Comma-separated allow-list; `None` keeps the permissive default for an open server.
    let cors_origins = std::env::var("DRAWER_CORS_ORIGINS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|origin| origin.trim().to_owned())
                .filter(|origin| !origin.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|origins| !origins.is_empty());
    if password.is_some() {
        eprintln!(
            "  auth   admin password + cookie sessions{}",
            if token.is_some() {
                " (machine bearer token enabled)"
            } else {
                ""
            }
        );
    } else if token.is_some() {
        eprintln!("  auth   bearer token required");
    } else if bound.ip().is_loopback() {
        eprintln!("  auth   none (loopback only)");
    } else {
        eprintln!(
            "  warning: no token set and listening beyond loopback; anyone who can reach this address can read and write"
        );
    }
    let cleanup = svc.clone();
    cleanup.prune_expired_transfers().await?;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            if let Err(e) = cleanup.prune_expired_transfers().await {
                eprintln!("drawer: unable to clean expired transfers: {e}");
            }
        }
    });
    // `into_make_service_with_connect_info` hands each request the peer address, which the
    // login limiter keys on. Callers that build the router themselves fall back gracefully.
    axum::serve(
        listener,
        crate::app_with_auth(svc, token, ui_dir, password, secure_cookie, cors_origins)
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// The UI is optional: ./ui/dist from a checkout, else the share directory the installer uses.
fn default_ui_dir() -> Option<PathBuf> {
    let local = PathBuf::from("ui/dist");
    if local.join("index.html").exists() {
        return Some(local);
    }
    let exe = std::env::current_exe().ok()?;
    let shared = exe.parent()?.join("../share/drawer/ui");
    shared.join("index.html").exists().then_some(shared)
}

fn mcp_config(format: McpFormat, data_dir: &Path, url: Option<&str>, name: &str) -> Result<String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        bail!("--name must be letters, digits, - or _, so every harness accepts it as a key");
    }
    let exe = std::env::current_exe()
        .context("finding the drawer binary")?
        .display()
        .to_string();
    let data = absolute(data_dir).display().to_string();
    let args = json!(["--data-dir", data, "stdio"]);
    // Inline list for YAML and TOML; JSON string escapes are valid in both.
    let args_list = format!("[\"--data-dir\", {}, \"stdio\"]", json!(data));
    // Placeholder where the client has no documented env interpolation.
    let auth = json!({ "Authorization": "Bearer YOUR_TOKEN" });
    let pretty = |v: serde_json::Value| -> Result<String> {
        Ok(format!("{}\n", serde_json::to_string_pretty(&v)?))
    };
    let sh = |s: &str| {
        if s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-:@=".contains(&b))
        {
            s.to_string()
        } else {
            format!("'{}'", s.replace('\'', "'\\''"))
        }
    };
    let token_note = "drop the Authorization header if the server runs without --token";
    Ok(match (format, url) {
        (McpFormat::Hermes, None) => format!(
            "# Add under mcp_servers in ~/.hermes/config.yaml\n\
             mcp_servers:\n  {name}:\n    command: {}\n    args: {args_list}\n",
            json!(exe)
        ),
        (McpFormat::Hermes, Some(url)) => format!(
            "# Add under mcp_servers in ~/.hermes/config.yaml; {token_note}\n\
             mcp_servers:\n  {name}:\n    url: {}\n    headers:\n      Authorization: \"Bearer YOUR_TOKEN\"\n",
            json!(url)
        ),
        (McpFormat::Json, None) => {
            pretty(json!({ "mcpServers": { name: { "command": exe, "args": args } } }))?
        }
        (McpFormat::Json, Some(url)) => {
            pretty(json!({ "mcpServers": { name: { "url": url, "headers": auth } } }))?
        }
        (McpFormat::ClaudeCode, None) => format!(
            "claude mcp add --scope user {} -- {} --data-dir {} stdio\n",
            sh(name),
            sh(&exe),
            sh(&data)
        ),
        (McpFormat::ClaudeCode, Some(url)) => format!(
            "# {token_note}\nclaude mcp add --scope user --transport http {} {} --header \"Authorization: Bearer $DRAWER_TOKEN\"\n",
            sh(name),
            sh(url)
        ),
        (McpFormat::Codex, None) => format!(
            "# Add to ~/.codex/config.toml\n[mcp_servers.{name}]\ncommand = {}\nargs = {args_list}\n",
            json!(exe)
        ),
        (McpFormat::Codex, Some(url)) => format!(
            "# Add to ~/.codex/config.toml; remove bearer_token_env_var if the server runs without --token\n\
             [mcp_servers.{name}]\nurl = {}\nbearer_token_env_var = \"DRAWER_TOKEN\"\n",
            json!(url)
        ),
        (McpFormat::Gemini, None) => pretty(
            json!({ "mcpServers": { name: { "command": exe, "args": args, "trust": false } } }),
        )?,
        (McpFormat::Gemini, Some(url)) => pretty(
            json!({ "mcpServers": { name: { "httpUrl": url, "headers": { "Authorization": "Bearer ${DRAWER_TOKEN}" } } } }),
        )?,
        (McpFormat::Vscode, None) => pretty(
            json!({ "servers": { name: { "type": "stdio", "command": exe, "args": args } } }),
        )?,
        (McpFormat::Vscode, Some(url)) => pretty(json!({
            "inputs": [{ "type": "promptString", "id": "drawer-token", "description": "Drawer bearer token", "password": true }],
            "servers": { name: { "type": "http", "url": url, "headers": { "Authorization": "Bearer ${input:drawer-token}" } } }
        }))?,
        (McpFormat::Opencode, None) => pretty(json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": { name: { "type": "local", "command": [exe, "--data-dir", data, "stdio"], "enabled": true } }
        }))?,
        (McpFormat::Opencode, Some(url)) => pretty(json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": { name: { "type": "remote", "url": url, "headers": { "Authorization": "Bearer {env:DRAWER_TOKEN}" }, "enabled": true } }
        }))?,
    })
}

async fn doctor(svc: &Drawer, data_dir: &Path, json: bool) -> Result<()> {
    let integrity = svc.integrity_check().await?;
    let stats = svc.stats().await?;
    let missing = svc.missing_blobs().await?;
    let ui = default_ui_dir();
    let healthy = integrity == "ok" && missing.is_empty();
    if json {
        emit_json(&json!({
            "healthy": healthy,
            "store": absolute(data_dir),
            "integrity": integrity,
            "stats": stats,
            "missing_blobs": missing,
            "ui": ui,
        }))?;
    } else {
        println!("store      {}", absolute(data_dir).display());
        println!("database   {integrity}");
        println!(
            "contents   {} files, {} folders, {} trashed, {} revisions",
            stats.files, stats.folders, stats.trashed, stats.revisions
        );
        if missing.is_empty() {
            println!("blobs      all revision blobs present");
        } else {
            println!("blobs      {} missing, e.g. {}", missing.len(), missing[0]);
        }
        match &ui {
            Some(dir) => println!("web UI     {}", dir.display()),
            None => println!("web UI     not built (serve still works without it)"),
        }
        println!("{}", if healthy { "healthy" } else { "problems found" });
    }
    if healthy {
        Ok(())
    } else {
        Err(anyhow!("store is not healthy"))
    }
}

async fn set_starred(
    svc: &Drawer,
    path: &str,
    starred: bool,
    actor: &str,
    json: bool,
) -> Result<()> {
    let node = svc.stat(Ref::Path(path)).await?;
    let node = svc
        .update(
            &node.id,
            NodeUpdate {
                starred: Some(starred),
                ..NodeUpdate::default()
            },
            actor,
        )
        .await?;
    if json {
        emit_json(&node)
    } else {
        println!(
            "{} {}",
            if starred { "starred" } else { "unstarred" },
            node.path
        );
        Ok(())
    }
}

/// Where an upload or copy lands. A destination that ends in `/`, or names an existing folder,
/// takes the item's own name inside it. Anything else is the full destination path.
async fn target_path(svc: &Drawer, dest: &str, name: &str) -> Result<String> {
    if dest.ends_with('/') {
        return Ok(format!("{dest}{name}"));
    }
    match svc.stat(Ref::Path(dest)).await {
        Ok(node) if node.kind == "folder" => {
            Ok(format!("{}/{name}", node.path.trim_end_matches('/')))
        }
        Ok(_) | Err(Error::NotFound(_)) => Ok(dest.to_string()),
        Err(e) => Err(e.into()),
    }
}

fn write_opts(
    actor: &str,
    comment: &str,
    props: Vec<(String, String)>,
    if_head_revision: Option<i64>,
) -> WriteOpts {
    let properties: Properties = props.into_iter().collect();
    WriteOpts {
        author: actor.into(),
        comment: comment.into(),
        properties,
        if_head_revision,
        required_parent: None,
    }
}

fn report_write(out: &WriteOutcome, json: bool) -> Result<()> {
    if json {
        return emit_json(out);
    }
    if out.new_revision {
        println!("{} saved as revision {}", out.node.path, out.revision);
    } else {
        println!(
            "{} unchanged, still revision {}",
            out.node.path, out.revision
        );
    }
    Ok(())
}

fn print_stat(n: &Node) {
    let row = |label: &str, value: String| println!("  {label:<10}{value}");
    println!("{}", n.path);
    row("id", n.id.clone());
    row("kind", n.kind.to_string());
    if let Some(rev) = n.head_revision {
        row("revision", rev.to_string());
    }
    if let Some(size) = n.size {
        row("size", human_size(size));
    }
    if let Some(mime) = &n.mime {
        row("type", mime.clone());
    }
    row("owner", n.owner.clone());
    row("modified", short_time(&n.modified_at));
    row("created", short_time(&n.created_at));
    row("starred", yes_no(n.starred));
    row("trashed", yes_no(n.trashed));
    for (k, v) in &n.properties {
        row("prop", format!("{k}={v}"));
    }
}

/// `tree`-style drawing of nodes from `walk`, which come depth first in path order.
fn print_tree(nodes: &[Node], root: &str) {
    let depth = |n: &Node| {
        n.path[root.trim_end_matches('/').len()..]
            .matches('/')
            .count()
    };
    for (i, n) in nodes.iter().enumerate() {
        let d = depth(n);
        let mut prefix = String::new();
        for level in 1..d {
            // A vertical bar continues while a later node sits at this level under the same parent.
            let open = nodes[i + 1..]
                .iter()
                .take_while(|m| depth(m) >= level)
                .any(|m| depth(m) == level);
            prefix.push_str(if open { "│   " } else { "    " });
        }
        let last = !nodes[i + 1..]
            .iter()
            .take_while(|m| depth(m) >= d)
            .any(|m| depth(m) == d);
        prefix.push_str(if last { "└── " } else { "├── " });
        let rev = n
            .head_revision
            .map(|r| format!("  (r{r}, {})", size_label(n)))
            .unwrap_or_default();
        println!("{prefix}{}{rev}", display_name(n));
    }
}

/// A unified diff, colored when stdout is a terminal.
fn print_diff(diff: &str) {
    use std::io::IsTerminal;
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    for line in diff.lines() {
        let code = match line.as_bytes().first() {
            _ if !color => None,
            Some(b'+') if !line.starts_with("+++") => Some("32"),
            Some(b'-') if !line.starts_with("---") => Some("31"),
            Some(b'@') => Some("36"),
            _ => None,
        };
        match code {
            Some(c) => println!("\x1b[{c}m{line}\x1b[0m"),
            None => println!("{line}"),
        }
    }
}

async fn memory(svc: &Drawer, action: MemoryAction, actor: &str, json: bool) -> Result<()> {
    use crate::vault;
    match action {
        MemoryAction::Remember {
            name,
            content,
            tag,
            append,
        } => {
            let content = match content {
                Some(c) => c,
                None => {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)?;
                    buf
                }
            };
            let out = vault::save(svc, &name, &content, &tag, append, actor).await?;
            if json {
                return emit_json(&out);
            }
            println!(
                "{} {}",
                if out["created"] == true {
                    "saved"
                } else {
                    "updated"
                },
                out["path"].as_str().unwrap_or_default()
            );
        }
        MemoryAction::Recall { query, tag, limit } => {
            let notes = vault::list(svc, tag.as_deref(), query.as_deref(), limit).await?;
            if json {
                return emit_json(&notes);
            }
            if notes.is_empty() {
                println!("no memories found");
            }
            for n in &notes {
                let tags: String = n.tags.iter().map(|t| format!(" #{t}")).collect();
                match n.snippet.as_deref() {
                    Some(s) if !s.is_empty() => println!("{}{tags}: {s}", n.name),
                    _ => println!("{}{tags}", n.name),
                }
            }
        }
        MemoryAction::Read { name } => {
            let d = vault::get(svc, &name).await?;
            if json {
                return emit_json(&d);
            }
            print!("{}", d.content);
            if !d.backlinks.is_empty() {
                println!(
                    "\nbacklinks: {}",
                    d.backlinks
                        .iter()
                        .map(|b| format!("[[{}]]", b.name))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        }
        MemoryAction::Forget { name } => {
            let out = vault::forget(svc, &name, actor).await?;
            if json {
                return emit_json(&out);
            }
            println!(
                "trashed {} (restore with: drawer restore {})",
                out["path"].as_str().unwrap_or_default(),
                out["id"].as_str().unwrap_or_default()
            );
        }
        MemoryAction::Graph => {
            let g = vault::graph(svc).await?;
            if json {
                return emit_json(&g);
            }
            if g.nodes.is_empty() {
                println!("no memories yet");
            }
            let name = |id: &str| {
                g.nodes
                    .iter()
                    .find(|n| n.id == id)
                    .map(|n| {
                        if n.path.is_some() {
                            n.name.clone()
                        } else {
                            format!("{}?", n.name)
                        }
                    })
                    .unwrap_or_default()
            };
            for n in g.nodes.iter().filter(|n| n.path.is_some()) {
                let out: Vec<String> = g
                    .edges
                    .iter()
                    .filter(|e| e.0 == n.id)
                    .map(|e| name(&e.1))
                    .collect();
                let back: Vec<String> = g
                    .edges
                    .iter()
                    .filter(|e| e.1 == n.id)
                    .map(|e| name(&e.0))
                    .collect();
                println!("{}", n.name);
                if !out.is_empty() {
                    println!("  -> {}", out.join(", "));
                }
                if !back.is_empty() {
                    println!("  <- {}", back.join(", "));
                }
            }
            let missing: Vec<&str> = g
                .nodes
                .iter()
                .filter(|n| n.path.is_none())
                .map(|n| n.name.as_str())
                .collect();
            if !missing.is_empty() {
                println!("\nlinked but not written: {}", missing.join(", "));
            }
        }
        MemoryAction::Tags => {
            let tags = vault::tags(svc).await?;
            if json {
                return emit_json(&tags);
            }
            for (t, n) in tags {
                println!("#{t}  {n}");
            }
        }
    }
    Ok(())
}

fn emit_json<T: Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn yes_no(b: bool) -> String {
    if b { "yes" } else { "no" }.to_string()
}

fn display_name(n: &Node) -> String {
    if n.kind == "folder" {
        format!("{}/", n.name)
    } else {
        n.name.clone()
    }
}

fn revision_label(n: &Node) -> String {
    n.head_revision.map_or("-".to_string(), |r| r.to_string())
}

fn size_label(n: &Node) -> String {
    n.size.map_or("-".to_string(), human_size)
}

/// `2026-10-15T01:40:00.000Z` as `2026-10-15 01:40`.
fn short_time(ts: &str) -> String {
    ts.replace('T', " ").chars().take(16).collect()
}

fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// The last segment of a Drawer path, ignoring a trailing slash.
fn leaf(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Keep the standalone store independent of the directory Drawer is launched from.
fn default_data_dir() -> Result<PathBuf> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|home| home.is_absolute())
        .map(|home| home.join(".drawer"))
        .ok_or_else(|| {
            anyhow!("cannot determine home directory; set --data-dir or DRAWER_DATA_DIR")
        })
}

fn default_author() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "cli".into())
}
