//! Bare mirrors of GitHub repositories, and throwaway worktrees of PR heads.
//!
//! Review runs never touch your own checkouts. Each repository gets one
//! bare mirror under the data directory; a run fetches the PR into it and
//! checks the head out as a detached worktree, which it removes afterwards.

use std::{
    collections::{HashMap, HashSet},
    fs::TryLockError,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{Arc, LazyLock, Mutex, PoisonError},
};

use color_eyre::{
    Section,
    eyre::{Result, WrapErr, eyre},
};
use sanic_core::{
    hold::{ClaudeLink, is_claude_path},
    pr::PrKey,
    repo::RepoName,
};
use tokio::process::Command;

use crate::diff::{CONTEXT, LineMap};

pub struct Mirrors {
    root: PathBuf,
}

/// Checkouts (the fetch and `worktree add`) into one mirror run one at a
/// time, by mirror path. Process-wide rather than per [`Mirrors`], since
/// the TUI's chats check out with their own alongside `serve`'s reviews;
/// [`lock_file`] does the same across processes, for `sanic-review chat`.
/// [`Worktree::remove`] doesn't take this lock, so a removal can run
/// alongside another run's checkout; `worktree add` locks its new entry
/// while creating it, so the removal's `worktree prune` skips it.
static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Mutex::default);

/// Where the file lock for `mirror` lives: beside it, as `<name>.git.lock`.
fn lock_path(mirror: &Path) -> PathBuf {
    let mut path = mirror.as_os_str().to_owned();
    path.push(".lock");
    PathBuf::from(path)
}

/// Takes an exclusive advisory lock (`flock`) on `path`, creating it and its
/// directory as needed, and waits for any other holder, in this process or
/// another, calling `on_wait` first if there is one. The lock is released
/// when the file is dropped.
pub async fn lock_file(path: &Path, on_wait: impl FnOnce()) -> Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .wrap_err_with(|| format!("creating {}", dir.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .wrap_err_with(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => return Ok(file),
        Err(TryLockError::WouldBlock) => on_wait(),
        Err(TryLockError::Error(err)) => {
            return Err(err).wrap_err_with(|| format!("locking {}", path.display()));
        }
    }
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        file.lock()
            .wrap_err_with(|| format!("locking {}", path.display()))?;
        Ok(file)
    })
    .await
    .wrap_err("waiting for a mirror lock")?
}

/// What [`Mirrors::checkout`] is doing, for a caller that shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Another checkout from the mirror, in this process or another, has
    /// it; this one waits for that to finish. Only reported when it does
    /// have to wait.
    Waiting,
    /// Fetching the PR, because the mirror lacks its head or base.
    Fetching,
    /// Adding the worktree.
    CheckingOut,
}

/// What changed in a PR since an earlier head: see
/// [`Worktree::interdiff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interdiff {
    /// The earlier head is an ancestor of the new one, on the same merge
    /// base: the diff between them.
    Diff(String),
    /// It isn't, as after a rebase or a merge of the base: a `git
    /// range-diff` of the PR's commits then against its commits now.
    RangeDiff(String),
}

impl Interdiff {
    #[must_use]
    pub fn text(&self) -> &str {
        let (Self::Diff(text) | Self::RangeDiff(text)) = self;
        text
    }
}

/// A PR head checked out for one run.
#[derive(Debug)]
pub struct Worktree {
    mirror: PathBuf,
    path: PathBuf,
    /// Where the PR branched from its base; GitHub diffs against this.
    merge_base: String,
    head_sha: String,
}

impl Mirrors {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn mirror_path(&self, repo: &RepoName) -> PathBuf {
        self.root
            .join(&repo.owner)
            .join(format!("{}.git", repo.name))
    }

    /// The mirrors a data dir keeps, under `mirrors/`.
    #[must_use]
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join("mirrors"))
    }

    /// Whether `repo`'s mirror has `commit`, a full or short SHA. Blocks
    /// on `git`, and never fetches.
    #[must_use]
    pub fn has_commit(&self, repo: &RepoName, commit: &str) -> bool {
        let mirror = self.mirror_path(repo);
        is_sha(commit)
            && mirror.is_dir()
            && git_blocking(
                &mirror,
                &["cat-file", "-e", &format!("{commit}^{{commit}}")],
            )
            .is_ok()
    }

    /// `path` as it is at `commit` in `repo`'s mirror, or `None` if the
    /// mirror doesn't have that commit, or that commit doesn't have that
    /// file (a submodule's entry isn't one). Blocks on `git`, and never
    /// fetches.
    pub fn file_at(&self, repo: &RepoName, commit: &str, path: &str) -> Result<Option<Vec<u8>>> {
        let mirror = self.mirror_path(repo);
        if !is_sha(commit) || !mirror.is_dir() {
            return Ok(None);
        }
        // One argument, so a path can't pass for an option. Asking its
        // type fails too if the mirror doesn't have the commit.
        let spec = format!("{commit}:{path}");
        let is_blob = git_blocking(&mirror, &["cat-file", "-t", &spec])
            .is_ok_and(|kind| kind.trim_ascii() == b"blob");
        if !is_blob {
            return Ok(None);
        }
        git_blocking(&mirror, &["cat-file", "blob", &spec])
            .map(Some)
            .wrap_err_with(|| format!("reading {path} at {commit} from {}", mirror.display()))
    }

    /// Removes a worktree of `repo`'s mirror whose [`Worktree`] was lost,
    /// e.g. to a panic. Does nothing if `path` doesn't exist.
    pub async fn remove_worktree(&self, repo: &RepoName, path: &Path) {
        remove_worktree(&self.mirror_path(repo), path).await;
    }

    fn lock(&self, repo: &RepoName) -> Arc<tokio::sync::Mutex<()>> {
        LOCKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(self.mirror_path(repo))
            .or_default()
            .clone()
    }

    /// Takes `repo`'s mirror for a fetch, in this process and across
    /// processes, telling `progress` once if it has to wait.
    async fn take(
        &self,
        repo: &RepoName,
        progress: &mut impl FnMut(Step),
    ) -> Result<(tokio::sync::OwnedMutexGuard<()>, std::fs::File)> {
        let lock = self.lock(repo);
        let mut waited = false;
        let guard = if let Ok(guard) = Arc::clone(&lock).try_lock_owned() {
            guard
        } else {
            waited = true;
            progress(Step::Waiting);
            lock.lock_owned().await
        };
        let file = lock_file(&lock_path(&self.mirror_path(repo)), || {
            if !waited {
                progress(Step::Waiting);
            }
        })
        .await?;
        Ok((guard, file))
    }

    /// Fetches `key`'s head and base into the mirror, which the caller has
    /// taken, unless it has both already.
    async fn fetch(
        &self,
        git_url: &str,
        key: &PrKey,
        head_sha: &str,
        base_sha: &str,
        progress: &mut impl FnMut(Step),
    ) -> Result<PathBuf> {
        let mirror = self.mirror_path(&key.repo);
        tokio::fs::create_dir_all(&mirror)
            .await
            .wrap_err_with(|| format!("creating {}", mirror.display()))?;
        // Every time, not just when the directory is new: `init` is a no-op
        // on a repo, and if an earlier one failed, `git -C` in a non-repo
        // directory would find and fetch into an enclosing repository.
        git(&mirror, &["init", "--bare", "--quiet"]).await?;

        // A SHA names the same commit forever, so a mirror that has both
        // has everything the checkout and the merge base need; nothing
        // reads the `refs/pull/N/head` a fetch updates.
        if !(has_commit(&mirror, head_sha).await && has_commit(&mirror, base_sha).await) {
            progress(Step::Fetching);
            let url = format!(
                "{}/{}/{}.git",
                git_url.trim_end_matches('/'),
                key.repo.owner,
                key.repo.name
            );
            let pull_ref = format!("+refs/pull/{0}/head:refs/pull/{0}/head", key.number);
            git(
                &mirror,
                &["fetch", "--quiet", "--no-tags", &url, &pull_ref, base_sha],
            )
            .await
            .wrap_err_with(|| format!("fetching {} from {url}", key.url()))?;
            if !has_commit(&mirror, head_sha).await {
                return Err(eyre!(
                    "{} head {head_sha} is gone; it was probably force-pushed away",
                    key.url()
                ));
            }
        }
        Ok(mirror)
    }

    /// What `key`'s `head_sha` on `base_sha` needs for its `.claude`
    /// check, from the mirror, fetching the head and base as a checkout
    /// does: every path any of the PR's commits touches, and the symlinks
    /// with a `.claude` component in the head's tree.
    pub async fn claude_tree(
        &self,
        git_url: &str,
        key: &PrKey,
        head_sha: &str,
        base_sha: &str,
    ) -> Result<ClaudeTree> {
        let mut progress = |_| {};
        let _taken = self.take(&key.repo, &mut progress).await?;
        let mirror = self
            .fetch(git_url, key, head_sha, base_sha, &mut progress)
            .await?;
        let merge_base = git(&mirror, &["merge-base", base_sha, head_sha])
            .await?
            .trim()
            .to_owned();
        Ok(ClaudeTree {
            touched: touched(&mirror, &merge_base, head_sha)
                .await
                .wrap_err_with(|| format!("listing what {} touches", key.url()))?,
            links: claude_links(&mirror, head_sha)
                .await
                .wrap_err_with(|| format!("reading the symlinks of {} at {head_sha}", key.url()))?,
        })
    }

    /// Fetches `key`'s head and base into the mirror from
    /// `<git_url>/<owner>/<name>.git`, unless it has both already, then
    /// checks `head_sha` out at `dest`, replacing anything a previous
    /// attempt left there. Tells `progress` each [`Step`] as it starts.
    pub async fn checkout(
        &self,
        git_url: &str,
        key: &PrKey,
        head_sha: &str,
        base_sha: &str,
        dest: &Path,
        mut progress: impl FnMut(Step),
    ) -> Result<Worktree> {
        let _taken = self.take(&key.repo, &mut progress).await?;
        let mirror = self
            .fetch(git_url, key, head_sha, base_sha, &mut progress)
            .await?;
        let merge_base = git(&mirror, &["merge-base", base_sha, head_sha])
            .await?
            .trim()
            .to_owned();

        remove_worktree(&mirror, dest).await;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .wrap_err_with(|| format!("creating {}", parent.display()))?;
        }
        progress(Step::CheckingOut);
        let dest_arg = dest.to_string_lossy();
        git(
            &mirror,
            &[
                "worktree", "add", "--detach", "--quiet", &dest_arg, head_sha,
            ],
        )
        .await
        .wrap_err_with(|| format!("checking out {} at {head_sha}", key.url()))?;
        Ok(Worktree {
            mirror,
            path: dest.to_owned(),
            merge_base,
            head_sha: head_sha.to_owned(),
        })
    }
}

impl Worktree {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The PR's diff as GitHub shows it: from the merge base to the head.
    pub async fn diff(&self) -> Result<String> {
        git(
            &self.mirror,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--no-color",
                "--no-ext-diff",
                // GitHub's context, whatever `diff.context` says.
                &format!("--unified={CONTEXT}"),
                "--src-prefix=a/",
                "--dst-prefix=b/",
                &self.merge_base,
                &self.head_sha,
            ],
        )
        .await
        .wrap_err("diffing the PR")
    }

    /// Where the PR branched from its base: the old side of its diff.
    #[must_use]
    pub fn merge_base(&self) -> &str {
        &self.merge_base
    }

    /// Where a PR whose head was `head`, on `base`, branched from it: the
    /// old side of its diff then.
    pub async fn merge_base_of(&self, base: &str, head: &str) -> Result<String> {
        Ok(git(&self.mirror, &["merge-base", base, head])
            .await
            .wrap_err_with(|| format!("finding where {head} left {base}"))?
            .trim()
            .to_owned())
    }

    /// Where `path`'s lines at `from` are at `to`.
    pub async fn line_map(&self, from: &str, to: &str, path: &str) -> Result<LineMap> {
        let diff = git(
            &self.mirror,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-renames",
                "--unified=0",
                from,
                to,
                "--",
                path,
            ],
        )
        .await
        .wrap_err_with(|| format!("diffing {path} from {from} to {to}"))?;
        Ok(LineMap::parse(&diff))
    }

    /// What changed in the PR since its head was `from_head`, on
    /// `from_base`, for a review that resumes the session of one of that
    /// head: the diff between the heads when the PR only gained commits on
    /// the same merge base, and otherwise a range-diff. `None` if the
    /// mirror no longer has both.
    pub async fn interdiff(&self, from_head: &str, from_base: &str) -> Result<Option<Interdiff>> {
        if !(has_commit(&self.mirror, from_head).await && has_commit(&self.mirror, from_base).await)
        {
            return Ok(None);
        }
        let from_merge_base = self.merge_base_of(from_base, from_head).await?;
        // A merge of the base into the PR moves its merge base, and the
        // diff between the heads would show the base's changes as the PR's.
        let ancestor = from_merge_base == self.merge_base
            && git(
                &self.mirror,
                &["merge-base", "--is-ancestor", from_head, &self.head_sha],
            )
            .await
            .is_ok();
        if ancestor {
            let diff = git(
                &self.mirror,
                &[
                    "-c",
                    "core.quotePath=false",
                    "diff",
                    "--no-color",
                    "--no-ext-diff",
                    &format!("--unified={CONTEXT}"),
                    "--src-prefix=a/",
                    "--dst-prefix=b/",
                    from_head,
                    &self.head_sha,
                ],
            )
            .await
            .wrap_err("diffing the PR since its last review")?;
            return Ok(Some(Interdiff::Diff(diff)));
        }
        let range_diff = git(
            &self.mirror,
            &[
                "-c",
                "core.quotePath=false",
                "range-diff",
                "--no-color",
                &format!("{from_merge_base}..{from_head}"),
                &format!("{}..{}", self.merge_base, self.head_sha),
            ],
        )
        .await
        .wrap_err("comparing the PR's commits with those of its last review")?;
        Ok(Some(Interdiff::RangeDiff(range_diff)))
    }

    /// Removes the worktree, best effort: a later checkout at the same path
    /// clears whatever is left. Doesn't take the mirror's checkout lock.
    pub async fn remove(self) {
        remove_worktree(&self.mirror, &self.path).await;
    }
}

async fn remove_worktree(mirror: &Path, path: &Path) {
    if path.exists() {
        let path_arg = path.to_string_lossy();
        if git(mirror, &["worktree", "remove", "--force", &path_arg])
            .await
            .is_err()
        {
            let _ = tokio::fs::remove_dir_all(path).await;
        }
    }
    let _ = git(mirror, &["worktree", "prune"]).await;
}

/// What a head's `.claude` check reads from the mirror.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeTree {
    /// Every path a commit from the merge base to the head touches, each
    /// once: a file changed and changed back still counts, as do both
    /// sides of a rename, and a merge's own resolutions.
    pub touched: Vec<String>,
    /// The symlinks with a `.claude` component in the head's tree.
    pub links: Vec<ClaudeLink>,
}

/// Every path a commit in `merge_base..head` touches. A merge counts
/// only for what it changes beyond its parents, so merging the base in
/// brings none of the base's paths.
async fn touched(mirror: &Path, merge_base: &str, head: &str) -> Result<Vec<String>> {
    let range = format!("{merge_base}..{head}");
    let out = git_bytes(
        mirror,
        &[
            "-c",
            "core.quotePath=false",
            "log",
            "--no-renames",
            "--cc",
            "--name-only",
            "--format=",
            "-z",
            &range,
        ],
    )
    .await?;
    let mut seen = HashSet::new();
    Ok(out
        .split(|&b| b == 0 || b == b'\n')
        .filter(|path| !path.is_empty() && seen.insert(*path))
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

/// The symlinks in `head`'s tree with a `.claude` component, the directory
/// itself included, at any depth, each with where it leads in the
/// repository, following links to links. One that leads outside the
/// repository, or round in a loop, is left out: the PR can't change what
/// it leads to.
async fn claude_links(mirror: &Path, head: &str) -> Result<Vec<ClaudeLink>> {
    let tree = git_bytes(mirror, &["ls-tree", "-r", "-z", "--full-tree", head]).await?;
    // Each entry is `<mode> <type> <object>\t<path>`, NUL-terminated.
    let links: HashMap<String, String> = tree
        .split(|&b| b == 0)
        .filter_map(|entry| {
            // Lossily, as `touched` reads paths, so a link at a path
            // that isn't UTF-8 is still found.
            let entry = String::from_utf8_lossy(entry);
            let (meta, path) = entry.split_once('\t')?;
            let mut meta = meta.split(' ');
            (meta.next()? == "120000").then_some(())?;
            Some((path.to_owned(), meta.nth(1)?.to_owned()))
        })
        .collect();
    let mut found = Vec::new();
    for link in links.keys().filter(|path| is_claude_path(path)) {
        if let Some(target) = resolve(mirror, &links, link).await? {
            found.push(ClaudeLink {
                link: link.clone(),
                target,
            });
        }
    }
    found.sort_by(|a, b| a.link.cmp(&b.link));
    Ok(found)
}

/// How many links a chain may pass through before it's taken for a loop:
/// Linux's own limit, so no chain a checkout would follow is left out.
const MAX_HOPS: usize = 40;

/// Where `link` leads in the repository, given every symlink in the tree
/// (`links`, path to blob), following links to links, including ones in
/// the middle of the path; `None` if it leads outside the repository or
/// loops.
async fn resolve(
    mirror: &Path,
    links: &HashMap<String, String>,
    link: &str,
) -> Result<Option<String>> {
    let mut path = link.to_owned();
    for _ in 0..MAX_HOPS {
        // The first component of `path` that's itself a link, if any.
        let parts: Vec<&str> = path.split('/').collect();
        let hop = (1..=parts.len()).find_map(|n| {
            let prefix = parts[..n].join("/");
            links.get(&prefix).map(|blob| (prefix, n, blob))
        });
        let Some((prefix, n, blob)) = hop else {
            return Ok(Some(path));
        };
        let target = git(mirror, &["cat-file", "blob", blob])
            .await
            .wrap_err_with(|| format!("reading the link {prefix}"))?;
        let parent = prefix.rsplit_once('/').map_or("", |(dir, _)| dir);
        let Some(resolved) = join_in_repo(parent, &target) else {
            return Ok(None);
        };
        let rest = parts[n..].join("/");
        path = [resolved, rest]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("/");
    }
    Ok(None)
}

/// `target`, a link's contents, from `dir`, as a path in the repository;
/// `None` if it's absolute or climbs out of the repository.
fn join_in_repo(dir: &str, target: &str) -> Option<String> {
    if target.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// Whether `mirror` has `commit` as a commit.
async fn has_commit(mirror: &Path, commit: &str) -> bool {
    git(mirror, &["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .await
        .is_ok()
}

/// Whether `commit` looks like a SHA, so it can only ever name one.
fn is_sha(commit: &str) -> bool {
    (4..=64).contains(&commit.len()) && commit.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `git` in `repo`. Never prompts for credentials. In its own process
/// group, so a Ctrl-C meant for a TUI chat doesn't reach a review's
/// checkout or the dashboard's reads.
fn git_command(repo: &Path, args: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .process_group(0);
    command
}

/// `git args`' stdout, as it is, if it ran and succeeded.
fn git_stdout(args: &[&str], output: std::io::Result<Output>) -> Result<Vec<u8>> {
    let output = output
        .wrap_err("running `git`")
        .suggestion("is `git` installed and on PATH?")?;
    if !output.status.success() {
        return Err(eyre!(
            "`git {}` exited with {}",
            args.join(" "),
            output.status
        ))
        .section(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(output.stdout)
}

/// Runs git in `repo` and waits, returning stdout as it is.
fn git_blocking(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    git_stdout(args, git_command(repo, args).output())
}

/// Runs git in `repo`, returning stdout as it is.
async fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::from(git_command(repo, args)).output().await;
    git_stdout(args, output)
}

/// Runs git in `repo`, returning stdout, which is decoded lossily because
/// diffs can hold any bytes.
async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::from(git_command(repo, args)).output().await;
    let stdout = git_stdout(args, output)?;
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}
