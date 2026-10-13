//! What `r` in the TUI and the dashboard's Review now do for a PR, which
//! both ask first: start a review, revise the one it has, or say why
//! there's nothing to start.

use crate::skip::Skip;

/// What `r` and Review now offer for a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewNow {
    /// Start a review of a PR you owe, for this reason.
    Start(Why),
    /// A review of it is already on its way, so there's nothing to start.
    Coming(Coming),
    /// Its latest review succeeded on its head, which hasn't moved since:
    /// another run is a revision of that review, with your instruction.
    Revise,
    /// Your own PR: nothing reviews it, and drafting replies to its
    /// feedback (`respond` runs) isn't built yet.
    Yours,
    /// Nothing can be started, e.g. its latest run was superseded.
    Nothing,
}

/// Why a review of a PR you owe may be started by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// The latest run failed or crashed.
    Failed,
    /// You cancelled its latest run, so nothing will start it again until
    /// the PR moves on.
    Cancelled,
    /// Manual reviews (`runner.manual_reviews`, or its profile's own) are
    /// holding its queued review.
    Held,
    /// It isn't reviewed automatically, for this reason: archived, a
    /// skipped author, a draft, already reviewed by someone, or a skipped
    /// title.
    Skipped(Skip),
    /// Its latest review succeeded, but the PR has moved past the head it
    /// reviewed, and no review of the new head is on its way.
    MovedOn,
    /// Nothing has reviewed it yet, and nothing is on its way to.
    Unreviewed,
}

/// Where a review that's on its way is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coming {
    /// Waiting out the quiet period before it's queued.
    Waiting,
    /// Queued, and waiting for its head's `.claude` check, after which it
    /// starts by itself if the check finds nothing.
    Checking,
    Queued,
    Running,
}

impl Coming {
    /// As in "A review of X is already …".
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting for the PR to go quiet",
            Self::Checking => "waiting until its head is checked for .claude changes",
            Self::Queued => "queued",
            Self::Running => "running",
        }
    }
}

/// What holds a queued run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Holding {
    /// Nothing: it starts by itself in its turn.
    #[default]
    Nothing,
    /// Only its head's `.claude` check, which manual reviews don't hold:
    /// it starts by itself once the check finds nothing.
    Check,
    /// Manual reviews or the `.claude` hold, until you start it.
    Held,
}

/// What decides [`ReviewNow`] for a PR you owe.
#[derive(Debug, Clone, Copy, Default)]
pub struct Owed<'a> {
    /// Why it isn't reviewed automatically, if it isn't
    /// ([`SkipRules::decide`](crate::skip::SkipRules::decide)'s answer).
    pub skip: Option<&'a Skip>,
    /// Its latest run's status.
    pub latest_run: Option<&'a str>,
    /// What holds its latest run, if that's queued.
    pub holding: Holding,
    /// Its latest run succeeded on a head it has since moved past.
    pub behind: bool,
    /// A review of it is waiting out the quiet period.
    pub waiting: bool,
    /// Its latest run is a revision of its head's review that failed,
    /// crashed or was cancelled: `serve` won't queue a head that has a
    /// succeeded review, so another run is a revision again.
    pub revision_failed: bool,
}

impl ReviewNow {
    /// For a PR you owe. A review on its way comes first, unless it's
    /// waiting out the quiet period while the PR is skipped, which it
    /// would be once it's due; then the skip, then the latest run. A
    /// skipped PR whose latest review succeeded on its head is revised,
    /// since `serve` won't queue that head again; so is one whose revision
    /// failed or was cancelled. A held run can be
    /// started while a review waits out the quiet period, which manual
    /// reviews would only hold too.
    #[must_use]
    pub fn owed(pr: Owed<'_>) -> Self {
        match (pr.skip, pr.latest_run) {
            (_, Some("running")) => Self::Coming(Coming::Running),
            (_, Some("queued")) if pr.holding == Holding::Check => Self::Coming(Coming::Checking),
            (_, Some("queued")) if pr.holding == Holding::Nothing => Self::Coming(Coming::Queued),
            (Some(_), Some("succeeded")) if !pr.behind => Self::Revise,
            (_, Some("failed" | "crashed" | "cancelled")) if pr.revision_failed => Self::Revise,
            (Some(skip), _) => Self::Start(Why::Skipped(skip.clone())),
            // Queued, and not caught above: held.
            (None, Some("queued")) => Self::Start(Why::Held),
            _ if pr.waiting => Self::Coming(Coming::Waiting),
            (None, Some("failed" | "crashed")) => Self::Start(Why::Failed),
            (None, Some("cancelled")) => Self::Start(Why::Cancelled),
            (None, Some("succeeded")) if pr.behind => Self::Start(Why::MovedOn),
            (None, Some("succeeded")) => Self::Revise,
            (None, None) => Self::Start(Why::Unreviewed),
            _ => Self::Nothing,
        }
    }

    /// Whether it starts a review, which `serve` does through `r`'s
    /// request.
    #[must_use]
    pub fn starts(&self) -> bool {
        matches!(self, Self::Start(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_revision_of_an_unchanged_review_is_revised_again() {
        for status in ["failed", "crashed", "cancelled"] {
            let failed = Owed {
                latest_run: Some(status),
                revision_failed: true,
                ..Owed::default()
            };
            assert_eq!(ReviewNow::owed(failed), ReviewNow::Revise, "{status}");
            let skip = Skip::Draft;
            assert_eq!(
                ReviewNow::owed(Owed {
                    skip: Some(&skip),
                    ..failed
                }),
                ReviewNow::Revise,
                "{status}, skipped"
            );
        }
    }

    fn latest(status: &str) -> Owed<'_> {
        Owed {
            latest_run: Some(status),
            ..Owed::default()
        }
    }

    #[test]
    fn a_review_on_its_way_comes_first_then_skips_then_the_latest_run() {
        let draft = Skip::Draft;
        let skipped = |status| {
            ReviewNow::owed(Owed {
                skip: Some(&draft),
                ..latest(status)
            })
        };
        assert_eq!(skipped("running"), ReviewNow::Coming(Coming::Running));
        assert_eq!(skipped("queued"), ReviewNow::Coming(Coming::Queued));
        assert_eq!(
            skipped("failed"),
            ReviewNow::Start(Why::Skipped(Skip::Draft))
        );
        // A skipped PR waiting out the quiet period would be skipped then.
        assert_eq!(
            ReviewNow::owed(Owed {
                skip: Some(&draft),
                waiting: true,
                ..Owed::default()
            }),
            ReviewNow::Start(Why::Skipped(Skip::Draft))
        );
        for status in ["failed", "crashed"] {
            assert_eq!(
                ReviewNow::owed(latest(status)),
                ReviewNow::Start(Why::Failed)
            );
        }
        assert_eq!(
            ReviewNow::owed(latest("cancelled")),
            ReviewNow::Start(Why::Cancelled)
        );
        let held = Owed {
            holding: Holding::Held,
            ..latest("queued")
        };
        assert_eq!(ReviewNow::owed(held), ReviewNow::Start(Why::Held));
        // Held only until its head's `.claude` check, it starts by itself.
        assert_eq!(
            ReviewNow::owed(Owed {
                holding: Holding::Check,
                ..held
            }),
            ReviewNow::Coming(Coming::Checking)
        );
        // A review waiting out the quiet period would only be held too.
        assert_eq!(
            ReviewNow::owed(Owed {
                waiting: true,
                ..held
            }),
            ReviewNow::Start(Why::Held)
        );
        assert_eq!(
            ReviewNow::owed(Owed::default()),
            ReviewNow::Start(Why::Unreviewed)
        );
        assert_eq!(ReviewNow::owed(latest("superseded")), ReviewNow::Nothing);
    }

    #[test]
    fn a_succeeded_review_is_revised_at_its_head_and_redone_past_it() {
        assert_eq!(ReviewNow::owed(latest("succeeded")), ReviewNow::Revise);
        // Skipped too: `serve` won't queue a head that has a succeeded review.
        let draft = Skip::Draft;
        assert_eq!(
            ReviewNow::owed(Owed {
                skip: Some(&draft),
                ..latest("succeeded")
            }),
            ReviewNow::Revise
        );
        let behind = Owed {
            behind: true,
            ..latest("succeeded")
        };
        assert_eq!(ReviewNow::owed(behind), ReviewNow::Start(Why::MovedOn));
        // Not while a review of the new head waits out the quiet period.
        assert_eq!(
            ReviewNow::owed(Owed {
                waiting: true,
                ..behind
            }),
            ReviewNow::Coming(Coming::Waiting)
        );
    }
}
