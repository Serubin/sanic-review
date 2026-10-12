-- Each `.claude` check of a PR's head on a base, by their SHAs, so a new
-- head, a retarget or a moved base is checked again. A review run's head
-- is checked when the run is queued; until its check is 'done', it never
-- starts by itself. 'failed' says why in `error`.
CREATE TABLE claude_checks (
    repo      TEXT    NOT NULL,
    number    INTEGER NOT NULL,
    base_sha  TEXT    NOT NULL,
    head_sha  TEXT    NOT NULL,
    state     TEXT    NOT NULL,
    error     TEXT,
    PRIMARY KEY (repo, number, base_sha, head_sha),
    FOREIGN KEY (repo, number) REFERENCES prs (repo, number)
);

-- What of a PR's changes reaches `.claude`, which holds its reviews for
-- good: rows are only ever added. `reason` is 'path' (`path` is a path a
-- commit touches with a `.claude` component) or 'link' (`path` is a path
-- a commit touches under where the `.claude` symlink `link` leads,
-- `target`). `source_*` is the PR whose changes they are, the held PR
-- itself for now; `head_sha` is the source's head that first showed it.
CREATE TABLE claude_holds (
    repo           TEXT    NOT NULL,
    number         INTEGER NOT NULL,
    source_repo    TEXT    NOT NULL,
    source_number  INTEGER NOT NULL,
    reason         TEXT    NOT NULL,
    path           TEXT    NOT NULL,
    link           TEXT    NOT NULL DEFAULT '',
    target         TEXT    NOT NULL DEFAULT '',
    head_sha       TEXT    NOT NULL,
    seen_at        TEXT    NOT NULL,
    PRIMARY KEY (repo, number, source_repo, source_number, reason, path),
    FOREIGN KEY (repo, number) REFERENCES prs (repo, number)
);
