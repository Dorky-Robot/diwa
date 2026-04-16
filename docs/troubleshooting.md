# Troubleshooting

Almost every hard-won bug fix in diwa came from a real-world failure mode that didn't surface in local testing. This doc catalogues the ones that have actually happened so you can diagnose them quickly if they happen again.

## Hooks silently stopped indexing

**Symptom:** you commit, but `diwa stats <repo>` shows no new insights. No visible error.

### Check the hook log first

```bash
tail -50 ~/.diwa/hooks.log
```

The hook log exists *precisely* for this problem — hooks are a deceptively hostile execution environment (unpredictable PATH, varying `core.hooksPath`, errors swallowed by default). If there's nothing in the log, the hook isn't firing at all. If there are errors, they'll point to the cause.

### Check for a stale binary earlier on PATH

The most common cause: an older `diwa` from a prior `cargo install` (commonly in `~/.local/bin`) sits earlier on `$PATH` than the current install. The hook resolves `diwa` via `command -v`, hits the stale binary, which doesn't know the `enqueue` subcommand, exits non-zero, and all indexing silently breaks.

```bash
which -a diwa
```

If multiple paths appear, the first one wins. `diwa update` probes the shadowing binary with `enqueue --help` — if the shadow is under `$HOME`, it renames it to `.stale-bak` so PATH falls through. Shadows outside `$HOME` get warnings only (won't touch system-managed files).

Fastest fix:

```bash
diwa update
```

Then retry a commit.

### Check for a custom `core.hooksPath`

If the repo uses Husky or another tool that sets `core.hooksPath` (e.g. to `.husky`), diwa's hook might be installed in `.git/hooks` but never fire because git reads from the configured path instead.

```bash
git config --get core.hooksPath
ls .git/hooks/post-commit .git/hooks/post-merge 2>/dev/null
ls .husky/post-commit .husky/post-merge 2>/dev/null
```

`diwa update` handles this case — it detects active vs inactive hook directories and cleans up stale hooks in the inactive location. If you ran a manual `git init` after installing diwa, re-run `diwa init` to re-install hooks in the active location.

### Check the daemon is running

```bash
diwa daemon status
```

If it says "Plist NOT installed" or the daemon isn't running, the hook is correctly enqueueing but nothing is draining the queue.

```bash
ls ~/.diwa/queue/
```

Any files sitting in there are unprocessed work. Fix:

```bash
diwa daemon install     # if the launchd plist is missing
launchctl kickstart -k gui/$(id -u)/com.dorky-robot.diwa   # if loaded but dead
```

Then:

```bash
tail -50 ~/.diwa/daemon.log
```

to see what the daemon reports.

## "Permission denied" on `diwa upgrade` (macOS)

**Symptom:** `diwa upgrade` fails with `Permission denied` when replacing `/usr/local/bin/diwa` or `/opt/homebrew/bin/diwa`.

**Cause:** macOS refuses to let you *write-open* a running binary, even if you have write permission to the file. `std::fs::copy` opens the target for writing, which fails.

**Fix:** This was patched in 0.4.8. The upgrader now `unlink`s the old binary before writing the new one — the kernel keeps the old inode mapped in memory until the running process exits, so the currently-running `diwa` is unaffected.

If you're on < 0.4.8 and hitting this, upgrade manually:

```bash
brew upgrade diwa
```

or re-download with the install script:

```bash
curl -fsSL https://raw.githubusercontent.com/Dorky-Robot/diwa/main/install.sh | sh
```

## ONNX Runtime dylib not found (Intel Mac)

**Symptom:** diwa panics on startup with a message about `libonnxruntime.dylib` on Intel Mac.

**Cause:** Intel Mac builds use `--features load-dynamic` because `ort-sys` ships no prebuilt binary for `x86_64-apple-darwin`. Load-dynamic means the dylib is looked up at runtime instead of linked at build time. If you installed via `diwa upgrade` rather than Homebrew, the dylib might not be on any linker search path.

**Fix in 0.4.7+:** diwa now probes `/usr/local/lib` and `/opt/homebrew/lib` at startup and sets `ORT_DYLIB_PATH` before the model init. The launchd plist also inherits this env var so the daemon finds the dylib.

If you still hit this, install onnxruntime via Homebrew:

```bash
brew install onnxruntime
```

That puts `libonnxruntime.dylib` in `/usr/local/lib` where diwa's auto-detection finds it.

## Indexing seems to skip recent commits

**Symptom:** `git log` shows commits that aren't in `diwa search` results.

### Are they too recent?

The daemon debounces — expect a few seconds of latency. Worst case, the 5-minute safety sweep will catch them.

```bash
tail ~/.diwa/daemon.log
```

### Was the hook installed when those commits landed?

Hooks only fire for commits made *after* `diwa init`. For commits that existed before init, `diwa init` runs a full backfill — but if you ran `diwa init` and then did a `git pull` that fast-forwarded *before* 0.4.0 landed post-merge hooks, those commits were never indexed.

Fix:

```bash
diwa index <repo>
```

`diwa index` is idempotent — it queries existing SHAs and only processes what's missing. Safe to re-run any time.

### Hard reset

If you want to nuke everything and rebuild:

```bash
diwa reindex <repo>
```

## Reflections feel stale

Reflections regenerate every 7 days. If you've had a big burst of commits (feature finish, refactor landing) and want a fresh retro-style view right now:

```bash
diwa reflect <repo>
```

Old reflections clear; new ones generate from the current insight set. Takes a minute or two because it's a grounded Claude call over the full insight corpus plus repo ground truth.

## Post-install silently fails on Homebrew

**Symptom:** Homebrew install succeeds but `diwa` isn't fully set up — hooks don't install in existing repos, daemon isn't registered.

**Cause:** Homebrew's sandbox prohibits writes outside the install prefix. A `post_install` hook that tried to write git hooks and `~/Library/LaunchAgents` plists hit 24 EPERM errors and was removed.

**Fix:** diwa self-heals on your next invocation instead. Run any command — `diwa ls`, `diwa stats`, anything — outside the sandbox and `auto_migrate_if_needed` fires transparently, setting up hooks and the daemon. A caveats block on the Homebrew formula explains this.

If auto-migrate didn't trigger for some reason:

```bash
diwa migrate
```

Runs it explicitly.

## Daemon commands fail on Linux

**Symptom:** `diwa daemon install` bails with "daemon install is currently macOS-only (launchd)."

**Cause:** as stated — launchd is macOS. Linux users need a systemd user unit pointing at `diwa daemon run`.

**Example systemd user unit** (`~/.config/systemd/user/diwa.service`):

```ini
[Unit]
Description=diwa background indexing daemon

[Service]
Type=simple
ExecStart=%h/.local/bin/diwa daemon run
Restart=on-failure

[Install]
WantedBy=default.target
```

Then:

```bash
systemctl --user daemon-reload
systemctl --user enable --now diwa
```

Adjust `ExecStart` to wherever your `diwa` binary lives.

## Getting more signal

In order of usefulness when debugging:

1. `~/.diwa/hooks.log` — did the hook fire?
2. `~/.diwa/daemon.log` — did the daemon pick it up?
3. `diwa daemon status` — is the daemon actually running?
4. `ls ~/.diwa/queue/` — are flag files piling up undrained?
5. `diwa stats <repo>` — what does the index think it has?
6. `which -a diwa` — is PATH shadowing biting you?

Most issues fall out of one of those six checks.
