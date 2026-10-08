//! What GitHub's answers mean for automations' triggers (see `schedule::Trigger`): each function
//! takes what one API call returned and gives the events in it. Which ones already ran is dinod's
//! to know (`TriggerState::seen`); here only those from before the trigger was set up (`since`)
//! are left out.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::schedule::{Event, TriggerKind, parse_time};

/// Comments dino posts carry this, so an automation that answers comments never answers its own.
pub const MARK: &str = "<!-- dino automation -->";

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn after(v: &Value, since: u64) -> bool {
    v.as_str().and_then(parse_time).is_some_and(|t| t >= since)
}

fn event(on: TriggerKind, key: String, title: String, url: &str, fields: Vec<(&str, String)>) -> Event {
    let fields: BTreeMap<String, String> = fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    Event { on, key, title, url: (!url.is_empty()).then(|| url.to_string()), fields }
}

fn pr_fields(p: &Value, repo: &str) -> Vec<(&'static str, String)> {
    vec![
        ("pr.number", s(&p["number"])),
        ("pr.title", s(&p["title"])),
        ("pr.url", s(&p["html_url"])),
        ("pr.author", s(&p["user"]["login"])),
        ("pr.branch", s(&p["head"]["ref"])),
        ("repo", repo.to_string()),
    ]
}

/// `GET /repos/{repo}/pulls`: pull requests opened since `since`.
pub fn prs_opened(pulls: &Value, repo: &str, since: u64) -> Vec<Event> {
    let mut out: Vec<Event> = pulls
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| after(&p["created_at"], since))
        .map(|p| {
            let n = s(&p["number"]);
            event(TriggerKind::PrOpened, format!("pr:{repo}#{n}"), format!("PR #{n} opened: {}", s(&p["title"])), &s(&p["html_url"]), pr_fields(p, repo))
        })
        .collect();
    // Oldest first, as they happened.
    out.reverse();
    out
}

/// `GET /repos/{repo}/pulls?state=closed`: pull requests merged since `since` (closed unmerged
/// ones aren't).
pub fn prs_merged(pulls: &Value, repo: &str, since: u64) -> Vec<Event> {
    let mut out: Vec<(u64, Event)> = pulls
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["merged_at"].as_str().and_then(parse_time).filter(|t| *t >= since)?, p)))
        .map(|(at, p)| {
            let n = s(&p["number"]);
            let mut fields = pr_fields(p, repo);
            fields.push(("pr.base", s(&p["base"]["ref"])));
            fields.push(("pr.sha", s(&p["merge_commit_sha"])));
            (at, event(TriggerKind::PrMerged, format!("merged:{repo}#{n}"), format!("PR #{n} merged: {}", s(&p["title"])), &s(&p["html_url"]), fields))
        })
        .collect();
    // Asked for by last update: run them in the order they merged.
    out.sort_by_key(|(at, _)| *at);
    out.into_iter().map(|(_, e)| e).collect()
}

/// `GET /repos/{repo}/pulls`: the open ones waiting on `me`'s review. Every PR still waiting is
/// in it, however long ago it was asked: dinod remembers which it ran for, and forgets one once
/// it no longer waits, so asking again runs again.
pub fn reviews_requested(pulls: &Value, repo: &str, me: &str) -> Vec<Event> {
    pulls
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["requested_reviewers"].as_array().is_some_and(|r| r.iter().any(|u| u["login"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(me)))))
        .map(|p| {
            let n = s(&p["number"]);
            event(TriggerKind::ReviewRequested, format!("review:{repo}#{n}"), format!("Review requested on PR #{n}: {}", s(&p["title"])), &s(&p["html_url"]), pr_fields(p, repo))
        })
        .collect()
}

/// `GET /search/issues?q=is:pr+is:open+review-requested:@me`: the same, in every repo.
pub fn reviews_requested_anywhere(found: &Value) -> Vec<Event> {
    found["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| {
            let repo = s(&i["repository_url"]).rsplitn(3, '/').take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/");
            let n = s(&i["number"]);
            let fields = vec![
                ("pr.number", n.clone()),
                ("pr.title", s(&i["title"])),
                ("pr.url", s(&i["html_url"])),
                ("pr.author", s(&i["user"]["login"])),
                ("pr.branch", String::new()),
                ("repo", repo.clone()),
            ];
            event(TriggerKind::ReviewRequested, format!("review:{repo}#{n}"), format!("Review requested on {repo}#{n}: {}", s(&i["title"])), &s(&i["html_url"]), fields)
        })
        .collect()
}

/// A commit's checks failing, from `GET /repos/{repo}/commits/{sha}/status` (commit statuses)
/// and `…/check-runs` (Actions and other apps): one event per commit, naming every failure.
/// `pr` is the PR the branch is for, if any: its number and address.
pub fn ci_failed(status: &Value, checks: &Value, repo: &str, branch: &str, sha: &str, pr: Option<(&str, &str)>, since: u64) -> Option<Event> {
    let mut failed: Vec<(String, String)> = vec![];
    for st in status["statuses"].as_array().into_iter().flatten() {
        if matches!(st["state"].as_str(), Some("failure" | "error")) && after(&st["updated_at"], since) {
            failed.push((s(&st["context"]), s(&st["target_url"])));
        }
    }
    for c in checks["check_runs"].as_array().into_iter().flatten() {
        if matches!(c["conclusion"].as_str(), Some("failure" | "timed_out" | "startup_failure")) && after(&c["completed_at"], since) {
            let url = [&c["html_url"], &c["details_url"]].into_iter().map(s).find(|u| !u.is_empty()).unwrap_or_default();
            failed.push((s(&c["name"]), url));
        }
    }
    if failed.is_empty() {
        return None;
    }
    let names: Vec<&str> = failed.iter().map(|f| f.0.as_str()).collect();
    let log = failed.iter().map(|f| f.1.clone()).find(|u| !u.is_empty()).unwrap_or_default();
    let short = &sha[..sha.len().min(7)];
    let (pr_number, pr_url) = pr.map_or((String::new(), String::new()), |(n, u)| (n.to_string(), u.to_string()));
    let fields = vec![
        ("ci.check", names.join(", ")),
        ("ci.log", log.clone()),
        ("ci.branch", branch.to_string()),
        ("ci.sha", sha.to_string()),
        ("pr.number", pr_number),
        ("pr.url", pr_url),
        ("repo", repo.to_string()),
    ];
    Some(event(TriggerKind::CiFailed, format!("ci:{repo}@{sha}"), format!("{} failed on {branch} ({short})", names.join(", ")), &log, fields))
}

/// `GET /repos/{repo}/issues/events`: issues and PRs labeled `label` since `since`.
pub fn labeled(events: &Value, repo: &str, label: &str, since: u64) -> Vec<Event> {
    let mut out: Vec<Event> = events
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["event"] == "labeled" && e["label"]["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(label.trim())) && after(&e["created_at"], since))
        .map(|e| {
            let i = &e["issue"];
            let n = s(&i["number"]);
            let fields = vec![("issue.number", n.clone()), ("issue.title", s(&i["title"])), ("issue.url", s(&i["html_url"])), ("label", s(&e["label"]["name"])), ("repo", repo.to_string())];
            event(TriggerKind::IssueLabeled, format!("label:{repo}:{}", s(&e["id"])), format!("#{n} labeled {}: {}", s(&e["label"]["name"]), s(&i["title"])), &s(&i["html_url"]), fields)
        })
        .collect();
    out.reverse();
    out
}

/// `GET /repos/{repo}/issues/comments` (and `/pulls/comments`, for comments on a PR's code):
/// comments made since `since` that say `phrase`. dino's own aren't counted (see `MARK`).
pub fn comments(list: &Value, repo: &str, phrase: &str, since: u64) -> Vec<Event> {
    let phrase = phrase.trim().to_lowercase();
    let mut out: Vec<Event> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| after(&c["created_at"], since))
        .filter(|c| c["body"].as_str().is_some_and(|b| !b.contains(MARK) && (phrase.is_empty() || b.to_lowercase().contains(&phrase))))
        .map(|c| {
            // An issue comment names its issue; a review comment its PR.
            let parent = [&c["issue_url"], &c["pull_request_url"]].into_iter().map(s).find(|u| !u.is_empty()).unwrap_or_default();
            let n = parent.rsplit('/').next().unwrap_or_default().to_string();
            let url = s(&c["html_url"]);
            let issue_url = url.split('#').next().unwrap_or_default().to_string();
            let body = s(&c["body"]);
            let fields = vec![
                ("comment.body", body.clone()),
                ("comment.url", url.clone()),
                ("comment.author", s(&c["user"]["login"])),
                ("issue.number", n.clone()),
                ("issue.title", String::new()),
                ("issue.url", issue_url),
                ("repo", repo.to_string()),
            ];
            let kind = if c["pull_request_url"].is_string() { "r" } else { "" };
            let line = body.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
            event(TriggerKind::Comment, format!("comment:{repo}:{kind}{}", s(&c["id"])), format!("{} on #{n}: {}", s(&c["user"]["login"]), crate::schedule::shorten(line, 80)), &url, fields)
        })
        .collect();
    // Asked for newest first; run oldest first.
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SINCE: u64 = 1_790_000_000; // 2026-09-21

    #[test]
    fn new_prs_only() {
        let pulls = json!([
            {"number": 7, "title": "New", "html_url": "https://github.com/o/r/pull/7", "created_at": "2026-10-04T10:00:00Z", "user": {"login": "amy"}, "head": {"ref": "fix", "sha": "abc"}},
            {"number": 3, "title": "Old", "html_url": "u", "created_at": "2026-01-01T00:00:00Z", "user": {"login": "bo"}, "head": {"ref": "old"}}
        ]);
        let e = prs_opened(&pulls, "o/r", SINCE);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].key, "pr:o/r#7");
        assert_eq!(e[0].fields["pr.url"], "https://github.com/o/r/pull/7");
        assert_eq!(e[0].fields["pr.branch"], "fix");
        assert_eq!(e[0].title, "PR #7 opened: New");
    }

    #[test]
    fn merged_prs_only() {
        let pulls = json!([
            {"number": 9, "title": "Later", "html_url": "u9", "merged_at": "2026-10-04T12:00:00Z", "merge_commit_sha": "m9", "user": {"login": "amy"}, "head": {"ref": "b9"}, "base": {"ref": "main"}},
            {"number": 8, "title": "Closed", "html_url": "u8", "merged_at": null, "user": {"login": "amy"}, "head": {"ref": "b8"}},
            {"number": 7, "title": "First", "html_url": "u7", "merged_at": "2026-10-04T10:00:00Z", "user": {"login": "bo"}, "head": {"ref": "b7"}, "base": {"ref": "main"}},
            {"number": 2, "title": "Old", "html_url": "u2", "merged_at": "2026-01-01T00:00:00Z", "user": {"login": "bo"}, "head": {"ref": "b2"}}
        ]);
        let e = prs_merged(&pulls, "o/r", SINCE);
        assert_eq!(e.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["merged:o/r#7", "merged:o/r#9"]);
        assert_eq!(e[1].title, "PR #9 merged: Later");
        assert_eq!(e[1].fields["pr.base"], "main");
        assert_eq!(e[1].fields["pr.sha"], "m9");
    }

    #[test]
    fn reviews_for_me() {
        let pulls = json!([
            {"number": 1, "title": "A", "html_url": "u1", "requested_reviewers": [{"login": "Me"}]},
            {"number": 2, "title": "B", "html_url": "u2", "requested_reviewers": [{"login": "you"}]}
        ]);
        let e = reviews_requested(&pulls, "o/r", "me");
        assert_eq!(e.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["review:o/r#1"]);
        let found = json!({"items": [{"number": 4, "title": "C", "html_url": "u4", "repository_url": "https://api.github.com/repos/x/y", "user": {"login": "z"}}]});
        let e = reviews_requested_anywhere(&found);
        assert_eq!(e[0].key, "review:x/y#4");
        assert_eq!(e[0].fields["repo"], "x/y");
    }

    #[test]
    fn ci_failures_from_statuses_and_checks() {
        let status = json!({"statuses": [
            {"state": "failure", "context": "ci/test", "target_url": "https://ci/1", "updated_at": "2026-10-04T10:00:00Z"},
            {"state": "success", "context": "ci/lint", "target_url": "https://ci/2", "updated_at": "2026-10-04T10:00:00Z"},
            {"state": "failure", "context": "ci/old", "target_url": "https://ci/3", "updated_at": "2026-01-01T00:00:00Z"}
        ]});
        let checks = json!({"check_runs": [{"name": "build", "conclusion": "failure", "html_url": "https://gh/run/9", "completed_at": "2026-10-04T10:01:00Z"}]});
        let e = ci_failed(&status, &checks, "o/r", "main", "abcdef123456", Some(("5", "https://pr/5")), SINCE).unwrap();
        assert_eq!(e.fields["ci.check"], "ci/test, build");
        assert_eq!(e.fields["ci.log"], "https://ci/1");
        assert_eq!(e.fields["pr.number"], "5");
        assert_eq!(e.key, "ci:o/r@abcdef123456");
        assert_eq!(e.title, "ci/test, build failed on main (abcdef1)");
        assert!(ci_failed(&json!({"statuses": []}), &json!({}), "o/r", "main", "abc", None, SINCE).is_none());
    }

    #[test]
    fn labels_by_name() {
        let ev = json!([
            {"id": 11, "event": "labeled", "label": {"name": "Bug"}, "created_at": "2026-10-04T10:00:00Z", "issue": {"number": 2, "title": "Crash", "html_url": "https://i/2"}},
            {"id": 12, "event": "labeled", "label": {"name": "docs"}, "created_at": "2026-10-04T10:00:00Z", "issue": {"number": 3, "title": "x", "html_url": "u"}},
            {"id": 13, "event": "closed", "created_at": "2026-10-04T10:00:00Z", "issue": {"number": 2}}
        ]);
        let e = labeled(&ev, "o/r", "bug", SINCE);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].key, "label:o/r:11");
        assert_eq!(e[0].fields["issue.number"], "2");
        assert_eq!(e[0].fields["label"], "Bug");
    }

    #[test]
    fn comments_that_say_the_phrase() {
        let list = json!([
            {"id": 5, "body": "hey @Dino please look", "html_url": "https://github.com/o/r/issues/4#issuecomment-5", "issue_url": "https://api.github.com/repos/o/r/issues/4", "user": {"login": "amy"}, "created_at": "2026-10-04T10:00:00Z"},
            {"id": 6, "body": "unrelated", "html_url": "u", "issue_url": "x/4", "user": {"login": "amy"}, "created_at": "2026-10-04T10:00:00Z"},
            {"id": 7, "body": format!("@dino said {MARK}"), "html_url": "u", "issue_url": "x/4", "user": {"login": "me"}, "created_at": "2026-10-04T10:00:00Z"}
        ]);
        let e = comments(&list, "o/r", "@dino", SINCE);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].fields["issue.number"], "4");
        assert_eq!(e[0].fields["issue.url"], "https://github.com/o/r/issues/4");
        assert_eq!(e[0].title, "amy on #4: hey @Dino please look");
    }
}
