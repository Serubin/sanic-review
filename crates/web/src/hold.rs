//! The warning on a PR with a `.claude` hold: on its index row, its page,
//! and the cards that start an agent on it.

use maud::{Markup, html};
use sanic_core::{
    hold::{CLAUDE_HOLD_WHY, ClaudeHold, Finding, HeadCheck},
    pr::PrKey,
};

use crate::pr::short;

/// The warning for `key`'s `hold`: each finding, the head that first
/// showed it and the PR whose changes it's in when that's another, and a
/// check of the current head that failed. A hold that only waits for its
/// head's check gets a quiet note instead.
pub fn warning(key: &PrKey, hold: &ClaudeHold) -> Markup {
    if !hold.warns() {
        return html! {
            p.claudewait.dim {
                "Checking this head's changes for " code { ".claude" }
                "; its review waits until that's done."
            }
        };
    }
    html! {
        div.claudehold role="alert" {
            strong {
                @if hold.findings.is_empty() {
                    "Held: this PR's changes to " code { ".claude" } " couldn't be checked."
                } @else {
                    "Held: this PR changes " code { ".claude" } "."
                }
            }
            " " (CLAUDE_HOLD_WHY)
            ul {
                @for found in &hold.findings {
                    li {
                        @match &found.finding {
                            Finding::Path(path) => { code { (path) } }
                            Finding::Link { path, link, target } => {
                                code { (path) } ", under " code { (target) } ", where "
                                code { (link) } " leads"
                            }
                        }
                        span.dim {
                            " · first at " code { (short(&found.head_sha)) }
                            @if &found.source != key {
                                " of " a href=(found.source.url()) { (found.source.url()) }
                            }
                        }
                    }
                }
                @if let HeadCheck::Failed(why) = &hold.head {
                    li {
                        "Checking its current head failed, and is tried again when "
                        code { "serve" } " next starts or a review of a new head is queued: " (why)
                    }
                }
            }
        }
    }
}
