//! The `.claude` hold: a PR whose changes touch a `.claude` directory
//! brings its own skills and agents, which load into the review agent and
//! can run shell commands as you, so its reviews never start by themselves.

use std::collections::HashSet;

use crate::pr::PrKey;

/// The directory Claude Code loads skills, agents and settings from.
const CLAUDE_DIR: &str = ".claude";

/// Those of `files`, changed paths relative to the repository root, with a
/// `.claude` component at any depth: `.claude/skills/x/SKILL.md` and
/// `sub/.claude/agents/y.md`, not `foo.claude/x` or `x.clauderc`. The
/// component matches in any case, since a case-insensitive filesystem
/// checks `.Claude` out as the directory Claude Code loads. In the order
/// given, each once.
#[must_use]
pub fn claude_paths<S: AsRef<str>>(files: &[S]) -> Vec<String> {
    let mut seen = HashSet::new();
    files
        .iter()
        .map(AsRef::as_ref)
        .filter(|file| is_claude_path(file) && seen.insert(*file))
        .map(str::to_owned)
        .collect()
}

/// Whether `path` has a `.claude` component, as [`claude_paths`] decides.
#[must_use]
pub fn is_claude_path(path: &str) -> bool {
    path.split('/')
        .any(|part| part.eq_ignore_ascii_case(CLAUDE_DIR))
}

/// A symlink with a `.claude` component and the path in the repository
/// it leads to, as the head's tree has them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeLink {
    pub link: String,
    pub target: String,
}

/// What of a PR's changes reaches `.claude`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// A changed path with a `.claude` component.
    Path(String),
    /// A changed path under where a `.claude` symlink leads.
    Link {
        path: String,
        link: String,
        target: String,
    },
}

/// What of `touched`, the paths a PR's commits touch, reaches `.claude`,
/// given the `.claude` symlinks in its head's tree: each path with a
/// `.claude` component, then each under where a link leads.
#[must_use]
pub fn findings<S: AsRef<str>>(touched: &[S], links: &[ClaudeLink]) -> Vec<Finding> {
    claude_paths(touched)
        .into_iter()
        .map(Finding::Path)
        .chain(link_findings(links, touched))
        .collect()
}

/// Those of `files` that the `links` lead to, or are under where they
/// lead: each file once, by the first link that reaches it. Paths match
/// in any case, as `.claude` does, since on a case-insensitive filesystem
/// a link to `Tools/agent` leads to `tools/agent`.
#[must_use]
pub fn link_findings<S: AsRef<str>>(links: &[ClaudeLink], files: &[S]) -> Vec<Finding> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for file in files {
        let file = file.as_ref();
        let reached = links.iter().find(|l| {
            let target = l.target.as_bytes();
            let file = file.as_bytes();
            target.is_empty()
                || (file.len() >= target.len()
                    && file[..target.len()].eq_ignore_ascii_case(target)
                    && (file.len() == target.len() || file[target.len()] == b'/'))
        });
        if let Some(link) = reached
            && seen.insert(file)
        {
            found.push(Finding::Link {
                path: file.to_owned(),
                link: link.link.clone(),
                target: link.target.clone(),
            });
        }
    }
    found
}

/// A [`Finding`] that holds a PR's reviews, and where it was seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeFinding {
    pub finding: Finding,
    /// The PR whose changes it's in: the held PR itself, or, once PRs can
    /// be paired across repositories, one paired with it.
    pub source: PrKey,
    /// The source's head that first showed it.
    pub head_sha: String,
}

/// How far a head's check for `.claude` changes has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadCheck {
    /// Not checked yet, or checked only in part.
    Pending,
    Done,
    /// Checking failed. It's tried again when a review of that head is
    /// next handed to the hold, as when `serve` next starts, not when the
    /// PR is polled.
    Failed(String),
}

/// Why a PR's reviews are held for `.claude`: what its changes reach,
/// which is sticky, since once a head shows it, it stays, whatever later
/// heads do; and whether its current head is checked yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeHold {
    /// In the order they were first seen.
    pub findings: Vec<ClaudeFinding>,
    /// The PR's current head's check.
    pub head: HeadCheck,
}

impl ClaudeHold {
    /// Whether it calls for the warning, rather than just a wait while the
    /// current head is checked.
    #[must_use]
    pub fn warns(&self) -> bool {
        !self.findings.is_empty() || matches!(self.head, HeadCheck::Failed(_))
    }

    /// The paths it names, for a line that lists them.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .findings
            .iter()
            .map(|f| match &f.finding {
                Finding::Path(path) => path.clone(),
                Finding::Link { path, link, .. } => format!("{path} via {link}"),
            })
            .collect();
        if let HeadCheck::Failed(why) = &self.head {
            paths.push(format!("couldn't check the head: {why}"));
        }
        paths
    }
}

/// Why a hold matters, for the warnings that show one. Plain text.
pub const CLAUDE_HOLD_WHY: &str = "A PR's own skills and agents load into the review agent, \
     and a skill can run shell commands as you, outside sanic-review's limits: they can read \
     anything you can, your GitHub token included. So this PR's reviews never start by \
     themselves. Read what it changes there before you start one.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claude_component_at_any_depth_counts_and_lookalikes_dont() {
        let files = [
            ".claude/skills/x/SKILL.md",
            "sub/dir/.claude/agents/y.md",
            "src/main.rs",
            "foo.claude/x",
            "x.clauderc",
            "docs/.claude",
            ".claude-pr/skills/z.md",
            ".claude/skills/x/SKILL.md",
            ".Claude/skills/z/SKILL.md",
        ];
        assert_eq!(
            claude_paths(&files),
            [
                ".claude/skills/x/SKILL.md",
                "sub/dir/.claude/agents/y.md",
                "docs/.claude",
                ".Claude/skills/z/SKILL.md",
            ]
        );
    }

    #[test]
    fn files_under_where_a_link_leads_are_found_and_lookalikes_arent() {
        let links = [
            ClaudeLink {
                link: ".claude".into(),
                target: "tools/agent".into(),
            },
            ClaudeLink {
                link: "pkg/.claude/skills/s".into(),
                target: "shared/skills/s.md".into(),
            },
        ];
        let files = [
            "tools/agent/skills/x/SKILL.md",
            "tools/agent-old/x",
            "tools/agent",
            "shared/skills/s.md",
            "src/lib.rs",
        ];
        let link = |path: &str, i: usize| Finding::Link {
            path: path.into(),
            link: links[i].link.clone(),
            target: links[i].target.clone(),
        };
        assert_eq!(
            link_findings(&links, &files),
            [
                link("tools/agent/skills/x/SKILL.md", 0),
                link("tools/agent", 0),
                link("shared/skills/s.md", 1),
            ]
        );
        // In any case, as a case-insensitive filesystem would have it.
        assert_eq!(link_findings(&links, &["Tools/Agent/x"]).len(), 1);
        // A link to the repository's root reaches everything.
        let root = [ClaudeLink {
            link: ".claude".into(),
            target: String::new(),
        }];
        assert_eq!(link_findings(&root, &["src/lib.rs"]).len(), 1);
    }

    #[test]
    fn a_rename_counts_from_either_side() {
        // `git log --no-renames` lists a rename as a delete and an add.
        assert_eq!(
            claude_paths(&["notes/agent.md", ".claude/agents/agent.md"]),
            [".claude/agents/agent.md"]
        );
        assert_eq!(
            claude_paths(&["skills/x.md", "pkg/.claude/skills/x.md"]),
            ["pkg/.claude/skills/x.md"]
        );
    }
}
