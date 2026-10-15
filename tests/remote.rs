//! `diwa remote`: a machine whose store lives elsewhere behaves as if local.
//!
//! The "remote" is a second HOME on this machine, reached through a stand-in
//! for ssh (`DIWA_SSH`) that runs the command line the way sshd would: in a
//! shell, with the remote HOME.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn diwa_bin() -> PathBuf {
    let mut p = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
    p.push("diwa");
    p
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

struct World {
    _tmp: tempfile::TempDir,
    hub_home: PathBuf,
    box_home: PathBuf,
    fake_ssh: PathBuf,
    sha: String,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let hub_home = tmp.path().join("hub-home");
    let box_home = tmp.path().join("box-home");
    std::fs::create_dir_all(&box_home).unwrap();

    // The hub: a store in a non-default dir with one registered repo.
    let repo = hub_home.join("src/widget");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "Cache the parser, because cold starts cost 2s"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    let store = hub_home.join("store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("repos.json"),
        format!("{{\"acme--widget\": \"{}\"}}", repo.display()),
    )
    .unwrap();

    // sshd stand-in: drop options and host, run the line in a shell with the hub's HOME.
    let fake_ssh = tmp.path().join("fake-ssh");
    std::fs::write(
        &fake_ssh,
        format!(
            "#!/bin/sh\nwhile [ \"$1\" = -o ]; do shift 2; done\n[ \"$1\" = -- ] && shift\nshift\nexec env HOME='{}' DIWA_EMBED=off sh -c \"$1\"\n",
            hub_home.display()
        ),
    )
    .unwrap();
    Command::new("chmod").arg("+x").arg(&fake_ssh).status().unwrap();
    World { _tmp: tmp, hub_home, box_home, fake_ssh, sha }
}

impl World {
    /// `diwa` on the machine that holds the store.
    fn on_hub(&self, args: &[&str]) -> Output {
        Command::new(diwa_bin())
            .args(args)
            .env("HOME", &self.hub_home)
            .env("DIWA_DIR", self.hub_home.join("store"))
            .env("DIWA_EMBED", "off")
            .env_remove("DIWA_REMOTE")
            .current_dir(&self.hub_home)
            .output()
            .unwrap()
    }

    /// `diwa` on another machine, with its default ~/.diwa.
    fn on_box(&self, args: &[&str]) -> Output {
        Command::new(diwa_bin())
            .args(args)
            .env("HOME", &self.box_home)
            .env("DIWA_SSH", &self.fake_ssh)
            .env("DIWA_EMBED", "off")
            .env_remove("DIWA_DIR")
            .env_remove("DIWA_REMOTE")
            .current_dir(&self.box_home)
            .output()
            .unwrap()
    }

    fn point_box_at_hub(&self) {
        let bin = diwa_bin();
        let o = self.on_box(&["remote", "set", "hub", "--bin", bin.to_str().unwrap(), "--dir", "store"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(String::from_utf8_lossy(&o.stdout).contains("now uses the store on hub"));
    }
}

fn out(o: &Output) -> String {
    assert!(o.status.success(), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn box_reads_and_curates_the_hub_store_as_if_local() {
    let w = world();
    out(&w.on_hub(&["insight", "add", "widget", &w.sha[..7], "Seed insight", "--body", "from the hub"]));
    w.point_box_at_hub();

    // Same bytes as on the hub (overview parses `ls`).
    assert_eq!(out(&w.on_box(&["ls"])), out(&w.on_hub(&["ls"])));
    assert_eq!(out(&w.on_box(&["stats", "widget"])), out(&w.on_hub(&["stats", "widget"])));
    let found = out(&w.on_box(&["search", "widget", "seed"]));
    assert!(found.contains("Seed insight"), "{found}");

    // Add from the box; it is in the hub's store.
    let added = out(&w.on_box(&["insight", "add", "acme/widget", &w.sha[..8], "Box-made note", "--tags", "it's quoted"]));
    assert!(added.contains("Added "), "{added}");
    let shown: serde_json::Value =
        serde_json::from_str(&out(&w.on_hub(&["show", "widget", &w.sha, "--json"]))).unwrap();
    let mine = shown["insights"].as_array().unwrap().iter()
        .find(|i| i["title"] == "Box-made note").expect("box insight on hub").clone();
    assert_eq!(mine["origin"], "manual");
    assert_eq!(mine["tags"], "it's quoted");
    let id = mine["uid"].as_str().unwrap()[..8].to_string();

    // Edit with the body on stdin, then remove, both from the box.
    let mut child = Command::new(diwa_bin())
        .args(["insight", "edit", &id, "--body", "-"])
        .env("HOME", &w.box_home)
        .env("DIWA_SSH", &w.fake_ssh)
        .env_remove("DIWA_DIR")
        .env_remove("DIWA_REMOTE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(b"-- a body that starts like a flag\n").unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    let shown = out(&w.on_box(&["show", "widget", &w.sha[..7]]));
    assert!(shown.contains("-- a body that starts like a flag"), "{shown}");
    assert!(out(&w.on_box(&["insight", "rm", &id])).contains("Removed"));
    assert!(!out(&w.on_hub(&["show", "widget", &w.sha])).contains("Box-made note"));

    // Errors come back as errors.
    let missing = w.on_box(&["insight", "rm", "ffffffff"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no insight with id"));
}

#[test]
fn indexing_stays_with_the_store_and_unset_goes_local() {
    let w = world();
    w.point_box_at_hub();
    let o = w.on_box(&["index", "."]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("indexing happens there"));

    assert!(out(&w.on_box(&["remote"])).contains("remote: hub"));
    assert!(out(&w.on_box(&["remote", "unset"])).contains("local store again"));
    assert!(out(&w.on_box(&["remote", "show"])).starts_with("local: "));
    assert!(out(&w.on_box(&["ls"])).contains("No indexed repos"));
}

#[test]
fn set_refuses_a_remote_that_does_not_answer() {
    let w = world();
    let o = w.on_box(&["remote", "set", "hub", "--bin", "/nonexistent/diwa"]);
    assert!(!o.status.success());
    assert!(out(&w.on_box(&["remote"])).starts_with("local: "), "nothing saved");
}
