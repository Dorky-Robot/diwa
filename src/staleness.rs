//! Is the index far enough behind the repo that its answers should be distrusted?
//!
//! Why this exists: on 2026-08-03 a `diwa search` against Dorky-Robot/kita was
//! answering from an index 1318 commits and six weeks stale. A husky v9 upgrade
//! had regenerated the directory the post-commit hook lived in, so nothing was
//! ever enqueued again. Every symptom was silence — search returned confident,
//! well-formed, *obsolete* answers, and nothing anywhere said so.
//!
//! A stale index is worse than a missing one: a missing index makes you go read
//! the code, while a stale index answers the question and you stop looking. So
//! the warning is emitted next to the results, at the moment of use, rather
//! than left to a discipline someone has to remember to perform.

use std::path::Path;
use std::process::Command;

/// Warn once the index is this far behind. Tight on purpose: the daemon
/// normally indexes within seconds of a commit, so a lag of ten commits or two
/// days already means the hook or the daemon is broken, not merely busy.
pub const COMMITS_THRESHOLD: usize = 10;
pub const DAYS_THRESHOLD: i64 = 2;

#[derive(Debug, PartialEq, Eq)]
pub enum Staleness {
    /// Index is at (or effectively at) HEAD.
    Fresh,
    /// Index trails HEAD by `commits` commits / `days` days.
    Behind { commits: usize, days: i64 },
    /// The indexed commit isn't in this repo's history at all — a rebase, a
    /// force-push, or the manifest pointing at the wrong checkout. Distinct
    /// from "behind" because re-running `diwa index` will NOT fix it.
    Detached,
}

/// Pure decision, split from the git calls so it can be tested directly.
pub fn classify(commits: usize, days: i64, reachable: bool) -> Staleness {
    if !reachable {
        return Staleness::Detached;
    }
    if commits >= COMMITS_THRESHOLD || days >= DAYS_THRESHOLD {
        Staleness::Behind { commits, days }
    } else {
        Staleness::Fresh
    }
}

/// The human-facing warning, or None when there's nothing worth saying.
pub fn warning(slug: &str, s: &Staleness) -> Option<String> {
    match s {
        Staleness::Fresh => None,
        Staleness::Behind { commits, days } => Some(format!(
            "⚠️  diwa index for {slug} is {commits} commits / {days} days BEHIND HEAD.\n\
             ⚠️  Answers below may predate recent work — verify against the code before\n\
             ⚠️  relying on them. Catch up with:  diwa index <repo-path>\n\
             ⚠️  If this keeps happening the post-commit hook is not firing (husky v9\n\
             ⚠️  repos: the hook must live in .husky/, never the regenerated .husky/_)."
        )),
        Staleness::Detached => Some(format!(
            "⚠️  diwa index for {slug} points at a commit that is NOT in this repo's\n\
             ⚠️  history (rebase, force-push, or the wrong checkout). Re-indexing will\n\
             ⚠️  not fix it — rebuild with:  diwa reindex <repo-path>"
        )),
    }
}

/// Measure the gap between `last_indexed` and HEAD in `repo_path`.
/// Returns `Fresh` when git can't answer — a warning we can't substantiate is
/// worse than none.
pub fn check(repo_path: &Path, last_indexed: &str) -> Staleness {
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };

    // Is the indexed commit an ancestor of HEAD at all?
    if git(&["merge-base", "--is-ancestor", last_indexed, "HEAD"]).is_none() {
        // Distinguish "unknown object" (detached) from "git unavailable" (quiet).
        return match git(&["cat-file", "-e", &format!("{last_indexed}^{{commit}}")]) {
            Some(_) => Staleness::Detached, // known commit, not an ancestor
            None => Staleness::Fresh,       // can't tell; stay quiet
        };
    }

    let commits = git(&["rev-list", "--count", &format!("{last_indexed}..HEAD")])
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    let ts = |rev: &str| {
        git(&["log", "-1", "--format=%ct", rev])?
            .parse::<i64>()
            .ok()
    };
    let days = match (ts(last_indexed), ts("HEAD")) {
        (Some(a), Some(b)) if b > a => (b - a) / 86_400,
        _ => 0,
    };

    classify(commits, days, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_when_at_head() {
        assert_eq!(classify(0, 0, true), Staleness::Fresh);
    }

    #[test]
    fn a_couple_of_commits_is_not_worth_shouting_about() {
        // The daemon is asynchronous; a small lag is normal, not a defect.
        assert_eq!(classify(3, 0, true), Staleness::Fresh);
    }

    #[test]
    fn commits_alone_can_trip_it() {
        assert_eq!(
            classify(COMMITS_THRESHOLD, 0, true),
            Staleness::Behind {
                commits: COMMITS_THRESHOLD,
                days: 0
            }
        );
    }

    #[test]
    fn time_alone_can_trip_it_on_a_quiet_repo() {
        // A repo with few commits can still go stale by sitting unindexed.
        assert_eq!(
            classify(1, DAYS_THRESHOLD, true),
            Staleness::Behind {
                commits: 1,
                days: DAYS_THRESHOLD
            }
        );
    }

    #[test]
    fn the_kita_incident_would_have_been_caught() {
        let s = classify(1318, 41, true);
        assert!(matches!(s, Staleness::Behind { .. }));
        let w = warning("Dorky-Robot/kita", &s).unwrap();
        assert!(w.contains("1318 commits"));
        assert!(w.contains("diwa index"));
    }

    #[test]
    fn unreachable_commit_reports_detached_and_says_reindex() {
        let s = classify(0, 0, false);
        assert_eq!(s, Staleness::Detached);
        let w = warning("x/y", &s).unwrap();
        assert!(w.contains("reindex"));
    }

    #[test]
    fn fresh_produces_no_warning() {
        assert!(warning("x/y", &Staleness::Fresh).is_none());
    }

    // --- the git plumbing, against a real repo -----------------------------

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo_with_commits(n: usize) -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        Command::new("git")
            .args(["init", tmp.path().to_str().unwrap()])
            .output()
            .unwrap();
        for i in 0..n {
            git(
                tmp.path(),
                &["commit", "--allow-empty", "-m", &format!("c{i}")],
            );
        }
        tmp
    }

    #[test]
    fn check_counts_real_commits_behind_head() {
        let repo = repo_with_commits(15);
        let base = git(repo.path(), &["rev-parse", "HEAD~12"]);
        match check(repo.path(), &base) {
            Staleness::Behind { commits, .. } => assert_eq!(commits, 12),
            other => panic!("expected Behind, got {other:?}"),
        }
    }

    #[test]
    fn check_is_fresh_at_head() {
        let repo = repo_with_commits(3);
        let head = git(repo.path(), &["rev-parse", "HEAD"]);
        assert_eq!(check(repo.path(), &head), Staleness::Fresh);
    }

    #[test]
    fn check_reports_detached_for_a_commit_on_an_abandoned_branch() {
        // A commit that exists but is not an ancestor of HEAD — what a rebase
        // or force-push leaves behind. Re-indexing cannot fix this, so the
        // warning has to say `reindex`, not `index`.
        let repo = repo_with_commits(2);
        git(repo.path(), &["checkout", "-b", "side"]);
        git(repo.path(), &["commit", "--allow-empty", "-m", "orphan"]);
        let orphan = git(repo.path(), &["rev-parse", "HEAD"]);
        git(repo.path(), &["checkout", "-"]);
        assert_eq!(check(repo.path(), &orphan), Staleness::Detached);
    }

    #[test]
    fn check_stays_quiet_when_git_cannot_answer() {
        // Not a git repo: we have no evidence, so we say nothing rather than
        // cry wolf.
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(check(tmp.path(), "deadbeef"), Staleness::Fresh);
    }
}
