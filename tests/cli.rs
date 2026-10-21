//! Runs the `drawer` binary against a temporary store, the way a person or an agent would.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn drawer(data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drawer"))
        .arg("--data-dir")
        .arg(data)
        .arg("--author")
        .arg("tester")
        .args(args)
        .output()
        .unwrap()
}

fn ok(out: Output) -> String {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn fail(out: Output) -> String {
    assert!(
        !out.status.success(),
        "expected failure, got stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8(out.stderr).unwrap()
}

/// Write a local file into `dir` and return its path.
fn local(dir: &Path, name: &str, body: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

#[test]
fn put_cat_and_ls_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let file = local(tmp.path(), "todo.md", b"# todo\n- ship it\n");

    let out = ok(drawer(&data, &["put", file.to_str().unwrap(), "/notes/"]));
    assert_eq!(out.trim(), "/notes/todo.md saved as revision 1");
    assert_eq!(
        ok(drawer(&data, &["cat", "/notes/todo.md"])),
        "# todo\n- ship it\n"
    );
    assert_eq!(ok(drawer(&data, &["ls"])), "notes/\n");
    assert_eq!(ok(drawer(&data, &["ls", "/notes"])), "todo.md\n");
}

#[test]
fn put_onto_an_existing_folder_lands_inside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    ok(drawer(&data, &["mkdir", "/docs"]));
    let file = local(tmp.path(), "a.txt", b"a");
    let out = ok(drawer(&data, &["put", file.to_str().unwrap(), "/docs"]));
    assert!(out.starts_with("/docs/a.txt"), "{out}");
}

#[test]
fn put_if_revision_guards_against_a_stale_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let v1 = local(tmp.path(), "v1.txt", b"one");
    let v2 = local(tmp.path(), "v2.txt", b"two");
    ok(drawer(
        &data,
        &["put", v1.to_str().unwrap(), "/a.txt", "--if-revision", "0"],
    ));
    // Creating again with --if-revision 0 must refuse, since the file exists.
    let err = fail(drawer(
        &data,
        &["put", v2.to_str().unwrap(), "/a.txt", "--if-revision", "0"],
    ));
    assert!(err.contains("precondition"), "{err}");
    let out = ok(drawer(
        &data,
        &["put", v2.to_str().unwrap(), "/a.txt", "--if-revision", "1"],
    ));
    assert!(out.contains("revision 2"), "{out}");
}

#[test]
fn put_reads_stdin_and_records_properties() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let mut child = Command::new(env!("CARGO_BIN_EXE_drawer"))
        .arg("--data-dir")
        .arg(&data)
        .args(["put", "-", "/from-stdin.txt", "--prop", "source=test"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"piped body")
        .unwrap();
    ok(child.wait_with_output().unwrap());

    assert_eq!(ok(drawer(&data, &["cat", "/from-stdin.txt"])), "piped body");
    let stat = ok(drawer(&data, &["--json", "stat", "/from-stdin.txt"]));
    assert!(stat.contains("\"source\": \"test\""), "{stat}");
}

#[test]
fn stdin_without_a_file_destination_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let mut child = Command::new(env!("CARGO_BIN_EXE_drawer"))
        .arg("--data-dir")
        .arg(&data)
        .args(["put", "-", "/notes/"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let err = fail(child.wait_with_output().unwrap());
    assert!(err.contains("destination file path"), "{err}");
}

#[test]
fn trash_restore_and_purge_by_id() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let file = local(tmp.path(), "gone.txt", b"bye");
    ok(drawer(&data, &["put", file.to_str().unwrap(), "/gone.txt"]));

    let out = ok(drawer(&data, &["rm", "/gone.txt"]));
    assert!(out.starts_with("trashed /gone.txt"), "{out}");
    assert_eq!(ok(drawer(&data, &["ls"])), "");

    let listing = ok(drawer(&data, &["--json", "trash"]));
    let entries: serde_json::Value = serde_json::from_str(&listing).unwrap();
    let id = entries[0]["id"].as_str().unwrap().to_string();
    assert_eq!(entries[0]["path"], "/gone.txt");

    // Purge needs --yes, and the node has to be in the trash first.
    let err = fail(drawer(&data, &["purge", &id]));
    assert!(err.contains("--yes"), "{err}");

    ok(drawer(&data, &["restore", &id]));
    assert_eq!(ok(drawer(&data, &["ls"])), "gone.txt\n");

    ok(drawer(&data, &["rm", "/gone.txt"]));
    ok(drawer(&data, &["purge", &id, "--yes"]));
    assert_eq!(ok(drawer(&data, &["trash"])).trim(), "trash is empty");
}

#[test]
fn history_and_rollback_keep_every_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    for body in [&b"one"[..], b"two", b"three"] {
        let f = local(tmp.path(), "rev.txt", body);
        ok(drawer(&data, &["put", f.to_str().unwrap(), "/rev.txt"]));
    }
    let history = ok(drawer(&data, &["--json", "history", "/rev.txt"]));
    let revs: serde_json::Value = serde_json::from_str(&history).unwrap();
    assert_eq!(revs.as_array().unwrap().len(), 3);

    let out = ok(drawer(&data, &["rollback", "/rev.txt", "1"]));
    assert!(out.contains("revision 4"), "{out}");
    assert_eq!(ok(drawer(&data, &["cat", "/rev.txt"])), "one");
    assert_eq!(
        ok(drawer(&data, &["cat", "/rev.txt", "--revision", "2"])),
        "two"
    );
}

#[test]
fn mv_and_cp_into_folders_keep_the_file_name() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let f = local(tmp.path(), "x.md", b"x");
    ok(drawer(&data, &["put", f.to_str().unwrap(), "/x.md"]));
    ok(drawer(&data, &["mkdir", "/archive"]));

    let out = ok(drawer(&data, &["cp", "/x.md", "/archive/"]));
    assert!(out.starts_with("/archive/x.md"), "{out}");
    assert_eq!(ok(drawer(&data, &["ls", "/"])), "archive/\nx.md\n");

    let g = local(tmp.path(), "y.md", b"y");
    ok(drawer(&data, &["put", g.to_str().unwrap(), "/y.md"]));
    let out = ok(drawer(&data, &["mv", "/y.md", "/archive"]));
    assert!(out.contains("-> /archive/y.md"), "{out}");
    assert_eq!(ok(drawer(&data, &["ls", "/archive"])), "x.md\ny.md\n");

    // Copying identical bytes onto an existing file adds no revision.
    let out = ok(drawer(&data, &["cp", "/x.md", "/archive/x.md"]));
    assert!(out.contains("unchanged, still revision 1"), "{out}");

    // Moving onto a name that is already taken is a conflict, never an overwrite.
    let err = fail(drawer(&data, &["mv", "/x.md", "/archive/x.md"]));
    assert!(err.contains("conflict"), "{err}");
}

#[test]
fn search_finds_names_and_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let f = local(tmp.path(), "plan.md", b"quarterly roadmap\nship the cli\n");
    ok(drawer(&data, &["put", f.to_str().unwrap(), "/plan.md"]));
    let out = ok(drawer(&data, &["search", "roadmap"]));
    assert!(out.contains("/plan.md: quarterly roadmap"), "{out}");
    let out = ok(drawer(&data, &["search", "PLAN"]));
    assert!(out.contains("/plan.md  (name)"), "{out}");
}

#[test]
fn get_refuses_to_overwrite_without_force() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let f = local(tmp.path(), "keep.txt", b"new");
    ok(drawer(&data, &["put", f.to_str().unwrap(), "/keep.txt"]));
    let target = local(tmp.path(), "out.txt", b"mine");
    let err = fail(drawer(
        &data,
        &["get", "/keep.txt", target.to_str().unwrap()],
    ));
    assert!(err.contains("--force"), "{err}");
    ok(drawer(
        &data,
        &["get", "/keep.txt", target.to_str().unwrap(), "--force"],
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
}

#[test]
fn read_commands_do_not_create_a_store() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("typo-dir");
    let err = fail(drawer(&data, &["ls"]));
    assert!(err.contains("no store at"), "{err}");
    assert!(!data.exists());
}

#[test]
fn star_and_perms() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let f = local(tmp.path(), "s.txt", b"s");
    ok(drawer(&data, &["put", f.to_str().unwrap(), "/s.txt"]));
    ok(drawer(&data, &["star", "/s.txt"]));
    assert_eq!(ok(drawer(&data, &["starred"])), "/s.txt\n");
    ok(drawer(&data, &["unstar", "/s.txt"]));
    assert_eq!(ok(drawer(&data, &["starred"])).trim(), "nothing starred");

    assert!(ok(drawer(&data, &["perms", "/s.txt"])).contains("open to anyone"));
    ok(drawer(&data, &["share", "/s.txt", "bob", "reader"]));
    assert!(ok(drawer(&data, &["perms", "/s.txt"])).contains("bob"));
    ok(drawer(&data, &["unshare", "/s.txt", "bob"]));
}

#[test]
fn doctor_passes_on_a_healthy_store_and_flags_a_missing_blob() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let f = local(tmp.path(), "d.txt", b"doctor me");
    ok(drawer(&data, &["put", f.to_str().unwrap(), "/d.txt"]));

    let report = ok(drawer(&data, &["doctor"]));
    assert!(report.contains("database   ok"), "{report}");
    assert!(report.contains("healthy"), "{report}");

    // Delete the blob behind the file. Doctor has to notice, and exit non-zero.
    let blob = walk_blobs(&data.join("blobs")).pop().expect("one blob");
    std::fs::remove_file(blob).unwrap();
    let err = fail(drawer(&data, &["doctor"]));
    assert!(err.contains("not healthy"), "{err}");
}

fn walk_blobs(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk_blobs(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[test]
fn mcp_config_points_at_this_binary_and_an_absolute_store() {
    let tmp = tempfile::tempdir().unwrap();
    let out = ok(Command::new(env!("CARGO_BIN_EXE_drawer"))
        .args(["--data-dir", "relative/store", "mcp-config"])
        .output()
        .unwrap());
    assert!(out.contains("mcp_servers:"), "{out}");
    assert!(out.contains("command: \""), "{out}");
    assert!(
        out.contains("\"--data-dir\", \"/"),
        "store path should be absolute: {out}"
    );
    assert!(out.contains("\"stdio\"]"), "{out}");

    let json = ok(Command::new(env!("CARGO_BIN_EXE_drawer"))
        .current_dir(tmp.path())
        .args(["--data-dir", "store", "mcp-config", "--format", "json"])
        .output()
        .unwrap());
    let cfg: serde_json::Value = serde_json::from_str(&json).unwrap();
    let args = &cfg["mcpServers"]["drawer"]["args"];
    assert_eq!(args[2], "stdio");
    assert!(args[1].as_str().unwrap().ends_with("/store"), "{args}");
}

#[test]
fn version_flag_prints_the_crate_version() {
    let out = ok(Command::new(env!("CARGO_BIN_EXE_drawer"))
        .arg("--version")
        .output()
        .unwrap());
    assert!(out.starts_with("drawer 0.1.3"), "{out}");
}

#[test]
fn edit_diff_tree_and_recent() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    let file = local(tmp.path(), "plan.md", b"step one\nstep two\n");
    ok(drawer(&data, &["put", file.to_str().unwrap(), "/work/"]));
    ok(drawer(&data, &["mkdir", "/work/empty"]));

    let out = ok(drawer(
        &data,
        &[
            "edit",
            "/work/plan.md",
            "--old",
            "two",
            "--new",
            "2",
            "-c",
            "shorter",
        ],
    ));
    assert!(out.contains("revision 2"), "{out}");
    let err = fail(drawer(
        &data,
        &["edit", "/work/plan.md", "--old", "step", "--new", "x"],
    ));
    assert!(err.contains("2 times"), "{err}");

    let diff = ok(drawer(&data, &["diff", "/work/plan.md"]));
    assert!(diff.contains("-step two\n+step 2"), "{diff}");

    let tree = ok(drawer(&data, &["tree"]));
    assert!(tree.contains("└── work/"), "{tree}");
    assert!(
        tree.contains("├── empty/") && tree.contains("└── plan.md"),
        "{tree}"
    );
    assert!(tree.contains("2 folders, 1 file"), "{tree}");

    let recent = ok(drawer(&data, &["recent"]));
    assert!(
        recent.contains("tester") && recent.contains("/work/plan.md"),
        "{recent}"
    );
}

#[test]
fn mcp_config_covers_each_harness_and_http() {
    let run = |args: &[&str]| {
        ok(Command::new(env!("CARGO_BIN_EXE_drawer"))
            .args(args)
            .output()
            .unwrap())
    };
    assert!(run(&["mcp-config", "--format", "claude-code"]).starts_with("claude mcp add"));
    let codex = run(&["mcp-config", "--format", "codex", "--url", "http://h:1/mcp"]);
    assert!(
        codex.contains("[mcp_servers.drawer]") && codex.contains("url = \"http://h:1/mcp\""),
        "{codex}"
    );
    let gemini: serde_json::Value = serde_json::from_str(&run(&[
        "mcp-config",
        "--format",
        "gemini",
        "--url",
        "http://h:1/mcp",
    ]))
    .unwrap();
    assert_eq!(gemini["mcpServers"]["drawer"]["httpUrl"], "http://h:1/mcp");
    let vscode: serde_json::Value = serde_json::from_str(&run(&[
        "mcp-config",
        "--format",
        "vscode",
        "--name",
        "files",
    ]))
    .unwrap();
    assert_eq!(vscode["servers"]["files"]["type"], "stdio");
    let opencode: serde_json::Value =
        serde_json::from_str(&run(&["mcp-config", "--format", "opencode"])).unwrap();
    assert_eq!(opencode["mcp"]["drawer"]["type"], "local");
    let err = fail(
        Command::new(env!("CARGO_BIN_EXE_drawer"))
            .args(["mcp-config", "--name", "a b"])
            .output()
            .unwrap(),
    );
    assert!(err.contains("--name"), "{err}");
    assert!(run(&["completions", "bash"]).contains("_drawer"));
}

#[test]
fn update_check_reports_an_unreachable_release_host_without_touching_the_store() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("store");
    // Port 9 is the discard service and is closed here, so the request fails at connect.
    let out = Command::new(env!("CARGO_BIN_EXE_drawer"))
        .arg("--data-dir")
        .arg(&data)
        .args(["update", "--check"])
        .env("DRAWER_API_URL", "http://127.0.0.1:9")
        .output()
        .unwrap();
    let err = fail(out);
    assert!(err.contains("cannot reach"), "{err}");
    assert!(!data.exists(), "update must not create a store");
}

/// Isolate home and working directory without changing this test process's environment.
fn standalone(home: &Path, cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drawer"));
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("DRAWER_DATA_DIR")
        .current_dir(cwd)
        .args(["--author", "tester"])
        .args(args);
    command
}

#[test]
fn default_store_is_shared_across_working_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home with spaces");
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    // An old working-directory store is never silently moved or deleted.
    std::fs::create_dir(first.join("data")).unwrap();
    std::fs::write(first.join("data/keep.txt"), "legacy data").unwrap();
    let file = local(tmp.path(), "note.txt", b"same home store\n");
    ok(
        standalone(&home, &first, &["put", file.to_str().unwrap(), "/note.txt"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        ok(standalone(&home, &second, &["cat", "/note.txt"])
            .output()
            .unwrap()),
        "same home store\n"
    );
    assert!(home.join(".drawer/drawer.db").is_file());
    assert!(home.join(".drawer/blobs").is_dir());
    assert_eq!(
        std::fs::read_to_string(first.join("data/keep.txt")).unwrap(),
        "legacy data"
    );
    assert!(!first.join("data/drawer.db").exists());
    assert!(!first.join(".drawer").exists());
    assert!(!second.join("data").exists());
    assert!(!second.join(".drawer").exists());
}

#[test]
fn explicit_store_overrides_environment_and_home_default() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let from_env = tmp.path().join("env-store");
    let from_flag = tmp.path().join("flag-store");
    ok(standalone(&home, tmp.path(), &["mkdir", "/environment"])
        .env("DRAWER_DATA_DIR", &from_env)
        .output()
        .unwrap());
    ok(standalone(
        &home,
        tmp.path(),
        &["--data-dir", from_flag.to_str().unwrap(), "mkdir", "/flag"],
    )
    .env("DRAWER_DATA_DIR", &from_env)
    .output()
    .unwrap());
    assert!(from_env.join("drawer.db").is_file());
    assert!(from_flag.join("drawer.db").is_file());
    assert_eq!(ok(drawer(&from_env, &["ls"])), "environment/\n");
    assert_eq!(ok(drawer(&from_flag, &["ls"])), "flag/\n");
    assert!(!home.exists());
}

#[test]
fn mcp_config_uses_home_store_without_creating_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let config = ok(
        standalone(&home, tmp.path(), &["mcp-config", "--format", "json"])
            .output()
            .unwrap(),
    );
    let value: serde_json::Value = serde_json::from_str(&config).unwrap();
    let args = &value["mcpServers"]["drawer"]["args"];
    assert_eq!(args[0], "--data-dir");
    assert_eq!(args[1], home.join(".drawer").to_str().unwrap());
    assert_eq!(args[2], "stdio");
    assert!(!home.exists());
}

#[test]
fn reading_missing_default_store_does_not_create_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let error = fail(standalone(&home, tmp.path(), &["ls"]).output().unwrap());
    assert!(error.contains("no store at"), "{error}");
    assert!(
        error.contains(&home.join(".drawer").display().to_string()),
        "{error}"
    );
    assert!(!home.exists());
}

#[test]
fn missing_home_requires_explicit_store_but_not_help_or_completions() {
    let tmp = tempfile::tempdir().unwrap();
    let without_home = |args: &[&str]| {
        let mut command = standalone(tmp.path(), tmp.path(), args);
        command.env_remove("HOME").env_remove("USERPROFILE");
        command
    };
    let error = fail(without_home(&["ls"]).output().unwrap());
    assert!(error.contains("cannot determine home directory"), "{error}");
    ok(without_home(&["--help"]).output().unwrap());
    ok(without_home(&["completions", "bash"]).output().unwrap());
    let store = tmp.path().join("explicit-store");
    ok(
        without_home(&["--data-dir", store.to_str().unwrap(), "mkdir", "/works"])
            .output()
            .unwrap(),
    );
    assert!(store.join("drawer.db").is_file());
    assert!(!tmp.path().join(".drawer").exists());
}

#[test]
fn userprofile_is_used_when_home_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("profile");
    ok(standalone(&home, tmp.path(), &["mkdir", "/works"])
        .env("HOME", "")
        .output()
        .unwrap());
    assert!(home.join(".drawer/drawer.db").is_file());
    assert!(!tmp.path().join(".drawer").exists());
}

#[test]
fn service_install_writes_unit_without_starting_or_creating_store() {
    if !cfg!(target_os = "linux") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    ok(
        standalone(&home, tmp.path(), &["service", "install", "--no-start"])
            .output()
            .unwrap(),
    );
    let unit = std::fs::read_to_string(home.join(".config/systemd/user/drawer.service")).unwrap();
    assert!(unit.contains(home.join(".drawer").to_str().unwrap()));
    assert!(!home.join(".drawer").exists());
    fail(
        standalone(&home, tmp.path(), &["service", "install", "--no-start"])
            .output()
            .unwrap(),
    );
}

#[test]
fn backup_schedule_can_be_inspected_without_starting_systemd() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_drawer"))
        .env("HOME", dir.path())
        .args([
            "--data-dir",
            dir.path().join("store").to_str().unwrap(),
            "backup",
            "schedule",
            "--directory",
            dir.path().join("backups").to_str().unwrap(),
            "--no-start",
        ])
        .output()
        .unwrap();
    ok(out);
    let units = dir.path().join(".config/systemd/user");
    let service = std::fs::read_to_string(units.join("drawer-backup.service")).unwrap();
    assert!(service.contains("backup scheduled --directory"));
    assert!(service.contains("UMask=0077"));
    assert!(
        std::fs::read_to_string(units.join("drawer-backup.timer"))
            .unwrap()
            .contains("OnCalendar=daily")
    );
    assert!(!dir.path().join("store").exists());
}

#[test]
fn memory_commands_save_recall_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    // `memory remember` starts the store, like `put`.
    assert!(
        ok(drawer(
            data,
            &[
                "memory",
                "remember",
                "Drawer",
                "A file server #infra",
                "-t",
                "project"
            ]
        ))
        .starts_with("saved /memory/Drawer.md")
    );
    ok(drawer(
        data,
        &["memory", "remember", "people/Alice", "Runs [[Drawer]]"],
    ));
    assert!(
        ok(drawer(data, &["memory", "recall", "file server"])).contains("Drawer #project #infra")
    );
    assert!(ok(drawer(data, &["memory", "recall", "-t", "project"])).contains("Drawer"));
    assert!(ok(drawer(data, &["memory", "read", "drawer"])).contains("backlinks: [[Alice]]"));
    assert!(ok(drawer(data, &["memory", "tags"])).contains("#infra  1"));
    let graph = ok(drawer(data, &["memory", "graph"]));
    assert!(
        graph.contains("Alice\n  -> Drawer") && graph.contains("Drawer\n  <- Alice"),
        "{graph}"
    );
    assert!(fail(drawer(data, &["memory", "read", "nope"])).contains("not found"));
    assert!(
        ok(drawer(data, &["memory", "forget", "Alice"]))
            .starts_with("trashed /memory/people/Alice.md")
    );
    assert!(fail(drawer(data, &["memory", "read", "Alice"])).contains("not found"));
}
