//! When a review of a PR you owe may be started by hand, which the TUI's
//! `r` and the dashboard's Review now both ask first.

use crate::skip::Skip;

/// Why a review may be started by hand.
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
}

impl Why {
    /// From why the PR isn't reviewed automatically, if it isn't
    /// ([`SkipRules::decide`](crate::skip::SkipRules::decide)'s answer),
    /// its latest run's status, whether manual reviews hold its profile's
    /// reviews, and whether it's `moved_on`: its latest run succeeded on
    /// an older head, and no review of the new one is waiting out the
    /// quiet period. `None` if there's nothing to start: its review is
    /// under way, done, or will come by itself.
    #[must_use]
    pub fn of(
        skip: Option<&Skip>,
        latest_run: Option<&str>,
        manual_reviews: bool,
        moved_on: bool,
    ) -> Option<Self> {
        match (skip, latest_run) {
            (Some(skip), _) => Some(Self::Skipped(skip.clone())),
            (None, Some("failed" | "crashed")) => Some(Self::Failed),
            (None, Some("cancelled")) => Some(Self::Cancelled),
            (None, Some("queued")) if manual_reviews => Some(Self::Held),
            (None, Some("succeeded")) if moved_on => Some(Self::MovedOn),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_come_first_then_failures_then_holds() {
        let draft = Skip::Draft;
        assert_eq!(
            Why::of(Some(&draft), Some("failed"), true, false),
            Some(Why::Skipped(Skip::Draft))
        );
        assert_eq!(
            Why::of(Some(&draft), Some("succeeded"), false, true),
            Some(Why::Skipped(Skip::Draft))
        );
        for status in ["failed", "crashed"] {
            assert_eq!(Why::of(None, Some(status), false, false), Some(Why::Failed));
        }
        assert_eq!(Why::of(None, Some("queued"), true, false), Some(Why::Held));
        // Without manual reviews a queued review runs by itself.
        assert_eq!(Why::of(None, Some("queued"), false, false), None);
        assert_eq!(
            Why::of(None, Some("cancelled"), false, false),
            Some(Why::Cancelled)
        );
        for status in [None, Some("running"), Some("succeeded"), Some("superseded")] {
            assert_eq!(Why::of(None, status, true, false), None, "{status:?}");
        }
    }

    #[test]
    fn a_review_the_pr_moved_past_can_be_started_again() {
        assert_eq!(
            Why::of(None, Some("succeeded"), false, true),
            Some(Why::MovedOn)
        );
        // Only once nothing is on its way: a queued or running review of
        // the new head is the latest run.
        for status in ["queued", "running"] {
            assert_eq!(Why::of(None, Some(status), false, true), None, "{status}");
        }
    }
}
