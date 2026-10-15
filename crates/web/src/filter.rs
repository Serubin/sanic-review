//! The index's filter: which rows the lists show, read from the URL's
//! query, and the sidebar that picks it. The server filters, so a filtered
//! index is a link and the lists' refresh keeps it.

use std::{collections::HashMap, fmt::Write};

use maud::{Markup, html};
use sanic_core::pr::is_login;

/// A group of values the sidebar picks from, and its key in the URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facet {
    Status,
    Author,
    Reviewer,
    Repo,
    State,
}

impl Facet {
    /// In the sidebar's order, which is also the URL's.
    pub const ALL: [Self; 5] = [
        Self::Status,
        Self::Author,
        Self::Reviewer,
        Self::Repo,
        Self::State,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Author => "author",
            Self::Reviewer => "reviewer",
            Self::Repo => "repo",
            Self::State => "state",
        }
    }

    fn of_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|facet| facet.key() == key)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Status => "Status",
            Self::Author => "Author",
            Self::Reviewer => "Reviewed by",
            Self::Repo => "Repo",
            Self::State => "State",
        }
    }

    /// The values a facet always has, in order, with their words; `None`
    /// for people and repos, which come from the rows.
    fn fixed(self) -> Option<&'static [(&'static str, &'static str)]> {
        match self {
            Self::Status => Some(STATUSES),
            Self::State => Some(STATES),
            Self::Author | Self::Reviewer | Self::Repo => None,
        }
    }

    /// Whether a pick of `value` filters `list`: your PRs are all yours, so
    /// author doesn't, and a status filters only the list that has it.
    pub fn applies(self, value: &str, list: Side) -> bool {
        match self {
            Self::Author => list == Side::Owed,
            Self::Status => list.statuses().contains(&value),
            Self::Reviewer | Self::Repo | Self::State => true,
        }
    }
}

/// The groups of both lists, as the Status facet names them. Needs you is
/// both lists' first group.
pub const STATUSES: &[(&str, &str)] = &[
    ("needs-you", "Needs you"),
    ("in-flight", "In flight"),
    ("nothing-to-do", "Nothing to do now"),
    ("ready", "Ready"),
    ("waiting", "Waiting on reviewers"),
    ("archived", "Archived"),
];

/// The [`STATUSES`] the reviews you owe have; an archived one is both
/// archived and nothing to do.
pub const OWED_STATUSES: &[&str] = &["needs-you", "in-flight", "nothing-to-do", "archived"];

/// The [`STATUSES`] your PRs have.
pub const MY_STATUSES: &[&str] = &["needs-you", "ready", "waiting", "archived"];

/// What the State facet can say of a row.
pub const STATES: &[(&str, &str)] = &[
    ("approved", "approved"),
    ("changes-requested", "changes requested"),
    ("mergeable", "mergeable"),
    ("ci-failing", "ci failing"),
    ("conflicts", "conflicts"),
    ("draft-pr", "draft PR"),
    ("unseen", "unseen review"),
];

/// How many of a facet's values show before the rest fold away.
const SHOWN: usize = 5;

/// Which list a row is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Owed,
    Mine,
}

impl Side {
    const ALL: [Self; 2] = [Self::Owed, Self::Mine];

    /// The list's id, which is also its tab's.
    fn key(self) -> &'static str {
        match self {
            Self::Owed => "owed",
            Self::Mine => "mine",
        }
    }

    fn words(self) -> &'static str {
        match self {
            Self::Owed => "reviews you owe",
            Self::Mine => "your PRs",
        }
    }

    fn statuses(self) -> &'static [&'static str] {
        match self {
            Self::Owed => OWED_STATUSES,
            Self::Mine => MY_STATUSES,
        }
    }
}

/// What the filter sees of a row.
#[derive(Debug, Clone)]
pub struct RowValues<'a> {
    pub list: Side,
    /// Its [`STATUSES`] values: its group's, and `archived` if it is.
    pub statuses: Vec<&'static str>,
    /// Someone else's PR's author; `None` for yours.
    pub author: Option<&'a str>,
    /// Whoever the status cell counts as having reviewed it.
    pub reviewers: Vec<&'a str>,
    /// `owner/name`.
    pub repo: String,
    /// [`STATES`] values.
    pub states: Vec<&'static str>,
    pub title: &'a str,
    /// `owner/name#N`.
    pub reference: String,
    pub pending: u32,
}

impl RowValues<'_> {
    fn values(&self, facet: Facet) -> Vec<&str> {
        match facet {
            Facet::Status => self.statuses.clone(),
            Facet::Author => self.author.into_iter().collect(),
            Facet::Reviewer => self.reviewers.clone(),
            Facet::Repo => vec![self.repo.as_str()],
            Facet::State => self.states.clone(),
        }
    }

    /// Logins and repos ignore case, as GitHub's do.
    fn has(&self, facet: Facet, value: &str) -> bool {
        self.values(facet).iter().any(|v| match facet {
            Facet::Author | Facet::Reviewer | Facet::Repo => is_login(v, value),
            Facet::Status | Facet::State => *v == value,
        })
    }
}

/// The values picked in each facet, and the words the title or ref must
/// have. Values in a facet are either-or; facets and words all apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// In the order picked, once each.
    picked: Vec<(Facet, String)>,
    /// As typed.
    text: String,
}

impl Filter {
    pub fn is_active(&self) -> bool {
        !self.picked.is_empty() || !self.text.trim().is_empty()
    }

    pub fn picked(&self, facet: Facet) -> impl Iterator<Item = &str> {
        self.picked
            .iter()
            .filter(move |(f, _)| *f == facet)
            .map(|(_, value)| value.as_str())
    }

    fn is_picked(&self, facet: Facet, value: &str) -> bool {
        self.picked(facet).any(|v| same(facet, v, value))
    }

    /// How many facets have a value picked, the words counting as one.
    fn in_use(&self) -> usize {
        let facets = Facet::ALL
            .iter()
            .filter(|facet| self.picked(**facet).next().is_some())
            .count();
        facets + usize::from(!self.text.trim().is_empty())
    }

    pub fn matches(&self, row: &RowValues) -> bool {
        self.matches_but(row, None)
    }

    /// [`Filter::matches`], leaving out what's picked in `skip`.
    fn matches_but(&self, row: &RowValues, skip: Option<Facet>) -> bool {
        for facet in Facet::ALL.into_iter().filter(|f| Some(*f) != skip) {
            let mut want = self
                .picked(facet)
                .filter(|v| facet.applies(v, row.list))
                .peekable();
            if want.peek().is_some() && !want.any(|v| row.has(facet, v)) {
                return false;
            }
        }
        let hay = format!("{} {}", row.title, row.reference).to_lowercase();
        self.text
            .split_whitespace()
            .all(|word| hay.contains(&word.to_lowercase()))
    }
}

fn same(facet: Facet, a: &str, b: &str) -> bool {
    match facet {
        Facet::Author | Facet::Reviewer | Facet::Repo => is_login(a, b),
        Facet::Status | Facet::State => a == b,
    }
}

/// The index's URL query: whether archived PRs show, and the filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexQuery {
    pub archived: bool,
    pub filter: Filter,
}

impl IndexQuery {
    /// From a URL's query: `archived=true`, each facet's key once per value
    /// (`author=alice&author=bob`), and `q` for the words. Anything else is
    /// left out.
    pub fn parse(query: Option<&str>) -> Result<Self, String> {
        let pairs: Vec<(String, String)> = serde_urlencoded::from_str(query.unwrap_or(""))
            .map_err(|err| format!("the index's query doesn't parse: {err}"))?;
        let mut out = Self::default();
        let mut words = Vec::new();
        for (key, value) in pairs {
            let value = value.trim();
            match key.as_str() {
                "archived" => match value {
                    "true" => out.archived = true,
                    "false" | "" => out.archived = false,
                    _ => return Err(format!("`archived={value}` isn't true or false")),
                },
                "q" if !value.is_empty() => words.push(value.to_owned()),
                _ if value.is_empty() => {}
                key => {
                    if let Some(facet) = Facet::of_key(key)
                        && !out.filter.is_picked(facet, value)
                    {
                        out.filter.picked.push((facet, value.to_owned()));
                    }
                }
            }
        }
        out.filter.text = words.join(" ");
        // Facets in the sidebar's order, as `href` writes them; a facet's
        // values stay in the order picked, which the URL keeps.
        out.filter
            .picked
            .sort_by_key(|(facet, _)| Facet::ALL.iter().position(|f| f == facet));
        Ok(out)
    }

    /// From an index URL a form sent back, as [`IndexQuery::href`] wrote
    /// it; the unfiltered index for anything else, so the redirect it
    /// makes stays on the index.
    pub fn from_href(href: Option<&str>) -> Self {
        let Some(query) = href.and_then(|h| h.strip_prefix('/')) else {
            return Self::default();
        };
        if query.is_empty() {
            return Self::default();
        }
        query
            .strip_prefix('?')
            .and_then(|q| Self::parse(Some(q)).ok())
            .unwrap_or_default()
    }

    /// The index's URL for this query: `/` when there's nothing to say.
    pub fn href(&self) -> String {
        let mut pairs: Vec<(&str, &str)> = Vec::new();
        if self.archived {
            pairs.push(("archived", "true"));
        }
        for facet in Facet::ALL {
            pairs.extend(self.filter.picked(facet).map(|v| (facet.key(), v)));
        }
        let text = self.filter.text.trim();
        if !text.is_empty() {
            pairs.push(("q", text));
        }
        match serde_urlencoded::to_string(&pairs) {
            Ok(query) if !query.is_empty() => format!("/?{query}"),
            _ => "/".into(),
        }
    }

    pub fn with_archived(&self, archived: bool) -> Self {
        Self {
            archived,
            filter: self.filter.clone(),
        }
    }

    /// This query with `facet`'s picks replaced by `values`, once each.
    pub fn with_facet(&self, facet: Facet, values: &[&str]) -> Self {
        let mut filter = self.filter.clone();
        filter.picked.retain(|(f, _)| *f != facet);
        for value in values {
            if !filter.is_picked(facet, value) {
                filter.picked.push((facet, (*value).to_owned()));
            }
        }
        filter
            .picked
            .sort_by_key(|(facet, _)| Facet::ALL.iter().position(|f| f == facet));
        Self {
            archived: self.archived,
            filter,
        }
    }

    pub fn unfiltered(&self) -> Self {
        Self {
            archived: self.archived,
            filter: Filter::default(),
        }
    }
}

/// `2 of 8` when the filter hides some of `total`, else `total`.
pub fn count(shown: usize, total: usize) -> String {
    if shown == total {
        total.to_string()
    } else {
        format!("{shown} of {total}")
    }
}

/// Under a list's heading: how many rows the filter hides, and a way to
/// clear it; `note` says what of the filter this list ignores.
pub fn hidden_line(query: &IndexQuery, hidden: usize, note: Option<&str>) -> Markup {
    html! {
        @if hidden > 0 || note.is_some() {
            p.fhidden {
                @if hidden > 0 {
                    b { (hidden) } " hidden by filter"
                    @if let Some(note) = note { " · " (note) }
                    " · " a.fclear href=(query.unfiltered().href()) { "clear" }
                } @else if let Some(note) = note {
                    (note)
                }
            }
        }
    }
}

/// What of the filter `list` ignores, for under its heading: the picks
/// that don't apply to it.
pub fn not_applied(filter: &Filter, list: Side, me: &str) -> Option<String> {
    let picks: Vec<String> = Facet::ALL
        .into_iter()
        .flat_map(|facet| {
            filter
                .picked(facet)
                .filter(move |v| !facet.applies(v, list))
                .map(move |v| words(facet, v, me))
        })
        .collect();
    (!picks.is_empty()).then(|| format!("not applied here: {}", picks.join(", ")))
}

/// One value of a facet in the sidebar.
struct Choice {
    value: String,
    words: String,
    /// Rows it would leave, under the other facets and the words.
    rows: usize,
    sides: [OnSide; 2],
    picked: bool,
}

/// A value as one list's tab shows it.
#[derive(Debug, Clone, Copy)]
struct OnSide {
    side: Side,
    /// Listed on the tab: the list has the value.
    on: bool,
    /// A pick of it filters the list.
    applies: bool,
    /// The list's rows it would leave.
    rows: usize,
}

impl Choice {
    fn side(&self, side: Side) -> Option<&OnSide> {
        self.sides.iter().find(|s| s.side == side)
    }

    fn on(&self, side: Side) -> bool {
        self.side(side).is_some_and(|s| s.on)
    }

    /// The lists it's a value of, as `data-on` spells them.
    fn on_keys(&self) -> String {
        sides_where(|side| self.on(side))
    }
}

/// `owed mine`, or as many of them as `f` keeps, for a `data-on`.
fn sides_where(f: impl Fn(Side) -> bool) -> String {
    let keys: Vec<&str> = Side::ALL
        .into_iter()
        .filter(|s| f(*s))
        .map(Side::key)
        .collect();
    keys.join(" ")
}

/// The sidebar: a form whose query is the index's, sent as you tick a
/// value or type. The lists' refresh replaces [`facets`] and the count in
/// its heading, never the text box, so typing isn't lost.
pub fn sidebar(query: &IndexQuery, rows: &[RowValues], me: &str) -> Markup {
    html! {
        form #filter .fside method="get" action="/" role="search" aria-label="Filter the lists"
            hx-get="/" hx-trigger="change, submit" hx-target="#panes" hx-select="#panes"
            hx-select-oob=(REFRESHED) hx-swap="outerHTML" hx-sync="#panes:replace" {
            // Open, and always so on a wide window; the script folds it on
            // a narrow one.
            details #filter-fold open {
                summary {
                    span.fsh { "Filter" } " " (in_use(&query.filter, me))
                    // Only where it folds: what a click on it does.
                    span.ftoggle aria-hidden="true" {}
                }
                div.fsbody {
                    input #fq type="search" name="q" value=(query.filter.text)
                        placeholder="title or #number" autocomplete="off"
                        aria-label="words in the title or owner/name#N"
                        hx-get="/" hx-trigger="input changed delay:250ms, search"
                        hx-include="#filter";
                    @if query.archived { input type="hidden" name="archived" value="true"; }
                    // Without the script's htmx, a plain GET.
                    button.btn.fapply type="submit" { "Filter" }
                    (facets(query, rows, me))
                }
            }
        }
    }
}

/// What the lists' refresh replaces besides them: the top bar's counts,
/// and the sidebar's facets and the count in its heading.
pub const REFRESHED: &str = "#counts,#facets,#filter-n";

/// The sidebar heading's word on the filter: how many facets are in use
/// beside the lists, and what's picked when the sidebar is over them,
/// where it folds away.
fn in_use(filter: &Filter, me: &str) -> Markup {
    let n = filter.in_use();
    let text = filter.text.trim();
    let picks: Vec<String> = Facet::ALL
        .into_iter()
        .flat_map(|facet| filter.picked(facet).map(move |v| words(facet, v, me)))
        .chain((!text.is_empty()).then(|| format!("“{text}”")))
        .collect();
    html! {
        span #filter-n .fcount {
            @if n > 0 {
                span.fnum { (n) " in use" }
                span.fpicks { (picks.join(", ")) }
            }
        }
    }
}

/// The facets, each value with how many rows it would leave. Everything
/// in them that takes focus has an id, so htmx puts focus back on it when
/// they're replaced.
///
/// Each value says which lists have it (`data-on`) and which a pick of it
/// filters (`data-applies`), and carries its count for each list and for
/// both (`data-side`); the stylesheet shows the open tab's, or both's
/// without the script.
pub fn facets(query: &IndexQuery, rows: &[RowValues], me: &str) -> Markup {
    let filter = &query.filter;
    html! {
        div #facets {
            @if filter.is_active() {
                a #fclear-all .fsclear.fclear href=(query.unfiltered().href()) { "clear all" }
            }
            @for facet in Facet::ALL {
                @let choices = choices(filter, facet, rows, me);
                @if !choices.is_empty() {
                    @let (shown, more) = fold(&choices);
                    fieldset.facet data-on=(sides_where(|s| choices.iter().any(|c| c.on(s)))) {
                        legend {
                            (facet.label())
                            @if facet == Facet::Author { " " span.dim { "reviews you owe" } }
                            (toggle(query, facet, &choices))
                        }
                        @for choice in shown { (option(facet, choice)) }
                        @if !more.is_empty() {
                            details.fmore id={ "facet-more-" (facet.key()) }
                                data-on=(sides_where(|s| more.iter().any(|c| c.on(s)))) {
                                summary id={ "facet-more-" (facet.key()) "-open" } {
                                    span data-side="all" { (more.len()) }
                                    @for side in Side::ALL {
                                        span data-side=(side.key()) {
                                            (more.iter().filter(|c| c.on(side)).count())
                                        }
                                    }
                                    " more"
                                }
                                @for choice in more { (option(facet, choice)) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A facet's link that ticks all its values, both lists' alike, or unticks
/// them once all are.
fn toggle(query: &IndexQuery, facet: Facet, choices: &[Choice]) -> Markup {
    let all = choices.iter().all(|c| c.picked);
    let values: Vec<&str> = if all {
        Vec::new()
    } else {
        choices.iter().map(|c| c.value.as_str()).collect()
    };
    html! {
        a.fall id={ "facet-all-" (facet.key()) } href=(query.with_facet(facet, &values).href())
            data-tick=(!all) {
            @if all { "none" } @else { "all" }
        }
    }
}

/// The values to show, the commonest and every picked one, and the rest,
/// to fold away.
fn fold(choices: &[Choice]) -> (Vec<&Choice>, Vec<&Choice>) {
    let (top, rest) = choices.split_at(choices.len().min(SHOWN));
    let shown = top.iter().chain(rest.iter().filter(|c| c.picked)).collect();
    (shown, rest.iter().filter(|c| !c.picked).collect())
}

fn option(facet: Facet, choice: &Choice) -> Markup {
    let id = format!("f-{}-{}", facet.key(), hex(&choice.value));
    let applies = |s: Side| choice.side(s).is_some_and(|s| s.applies);
    let zero = sides_where(|s| choice.side(s).is_some_and(|s| s.applies && s.rows == 0));
    // Shown on a tab whose list it doesn't filter, which only a pick is.
    let tag: Vec<&str> = Side::ALL
        .into_iter()
        .filter(|s| applies(*s))
        .map(Side::words)
        .collect();
    html! {
        label.fopt.zero[choice.rows == 0].picked[choice.picked] for=(id)
            data-on=(choice.on_keys()) data-applies=(sides_where(applies)) data-zero=(zero) {
            input id=(id) type="checkbox" name=(facet.key()) value=(choice.value)
                checked[choice.picked];
            span.fv {
                @if facet == Facet::State && choice.value == "unseen" { span.newdot {} " " }
                (choice.words)
                @if tag.len() == 1 { " " span.ftag { "· " (tag.join("")) } }
            }
            span.fn data-side="all" { (choice.rows) }
            @for s in &choice.sides {
                span.fn data-side=(s.side.key()) { @if s.applies { (s.rows) } }
            }
        }
    }
}

/// How the sidebar words one of a facet's values.
fn words(facet: Facet, value: &str, me: &str) -> String {
    match facet.fixed() {
        Some(fixed) => fixed
            .iter()
            .find(|(v, _)| *v == value)
            .map_or_else(|| value.to_owned(), |(_, w)| (*w).to_owned()),
        None if facet == Facet::Repo => value.to_owned(),
        None if is_login(value, me) => "you".into(),
        None => format!("@{value}"),
    }
}

/// An id-safe spelling of `value` that's the same on every render, so
/// focus stays on a box the refresh replaced.
fn hex(value: &str) -> String {
    value.bytes().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// A facet's values: the fixed ones in order, else the commonest first,
/// among the rows, and whatever is picked.
fn choices(filter: &Filter, facet: Facet, rows: &[RowValues], me: &str) -> Vec<Choice> {
    // How often each value comes up, by its lowercase spelling for logins
    // and repos, keeping the first spelling seen.
    let mut seen: Vec<(String, usize)> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut note = |value: &str, n: usize| {
        let fold = match facet {
            Facet::Author | Facet::Reviewer | Facet::Repo => value.to_ascii_lowercase(),
            Facet::Status | Facet::State => value.to_owned(),
        };
        if let Some(i) = at.get(&fold) {
            seen[*i].1 += n;
        } else {
            at.insert(fold, seen.len());
            seen.push((value.to_owned(), n));
        }
    };
    for row in rows {
        for value in row.values(facet) {
            note(value, 1);
        }
    }
    for value in filter.picked(facet) {
        note(value, 0);
    }
    match facet.fixed() {
        Some(fixed) => seen.sort_by_key(|(value, _)| {
            fixed
                .iter()
                .position(|(v, _)| v == value)
                .unwrap_or(fixed.len())
        }),
        None => seen.sort_by(|(a, m), (b, n)| n.cmp(m).then_with(|| a.cmp(b))),
    }
    seen.into_iter()
        .map(|(value, _)| {
            let leaves =
                |r: &&RowValues| r.has(facet, &value) && filter.matches_but(r, Some(facet));
            let side = |side: Side| {
                let of_side = || rows.iter().filter(|r| r.list == side);
                // People and repos are a list's when one of its rows has them.
                let applies = facet.applies(&value, side);
                OnSide {
                    side,
                    on: applies
                        && (facet.fixed().is_some() || of_side().any(|r| r.has(facet, &value))),
                    applies,
                    rows: of_side().filter(leaves).count(),
                }
            };
            let words = words(facet, &value, me);
            Choice {
                picked: filter.is_picked(facet, &value),
                rows: rows.iter().filter(leaves).count(),
                sides: Side::ALL.map(side),
                value,
                words,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row<'a>(list: Side, author: Option<&'a str>, reviewers: Vec<&'a str>) -> RowValues<'a> {
        RowValues {
            list,
            statuses: vec!["needs-you"],
            author,
            reviewers,
            repo: "org/repo".into(),
            states: vec!["approved"],
            title: "Add the retry budget",
            reference: "org/repo#7".into(),
            pending: 0,
        }
    }

    fn query(q: &str) -> IndexQuery {
        IndexQuery::parse(Some(q)).unwrap()
    }

    #[test]
    fn the_query_round_trips_in_the_sidebars_order() {
        let q = query("q=retry&author=bob&archived=true&author=Alice&status=ready&author=alice");
        assert_eq!(
            q.href(),
            "/?archived=true&status=ready&author=bob&author=Alice&q=retry"
        );
        assert_eq!(IndexQuery::from_href(Some(&q.href())), q);
        assert_eq!(query("author=&q=+&archived=false").href(), "/");
        assert!(IndexQuery::parse(Some("archived=yes")).is_err());
    }

    #[test]
    fn a_facets_toggle_ticks_every_value_then_none() {
        let rows: Vec<_> = ["alice", "bob", "carol", "dave", "erin", "frank"]
            .into_iter()
            .map(|a| row(Side::Owed, Some(a), vec![]))
            .collect();
        let toggle_of = |q: &IndexQuery| {
            let choices = choices(&q.filter, Facet::Author, &rows, "me");
            toggle(q, Facet::Author, &choices).into_string()
        };
        // The sixth author is folded away, and still ticked.
        let some = query("author=BOB&reviewer=x&q=retry");
        assert!(toggle_of(&some).contains(
            "href=\"/?author=alice&amp;author=bob&amp;author=carol&amp;author=dave\
             &amp;author=erin&amp;author=frank&amp;reviewer=x&amp;q=retry\" data-tick=\"true\">all<"
        ));
        let all = query(
            "author=alice&author=bob&author=carol&author=dave&author=erin&author=frank&q=retry",
        );
        assert!(toggle_of(&all).contains("href=\"/?q=retry\" data-tick=\"false\">none<"));
    }

    fn with_statuses<'a>(mut row: RowValues<'a>, statuses: &[&'static str]) -> RowValues<'a> {
        row.statuses = statuses.to_vec();
        row
    }

    #[test]
    fn a_status_filters_only_the_list_that_has_it() {
        let owed_quiet = with_statuses(row(Side::Owed, Some("a"), vec![]), &["nothing-to-do"]);
        let owed_need = row(Side::Owed, Some("a"), vec![]);
        let mine_ready = with_statuses(row(Side::Mine, None, vec![]), &["ready"]);
        let mine_need = row(Side::Mine, None, vec![]);
        let ready = query("status=ready").filter;
        assert!(ready.matches(&owed_quiet) && ready.matches(&owed_need));
        assert!(ready.matches(&mine_ready) && !ready.matches(&mine_need));
        let mixed = query("status=needs-you&status=ready").filter;
        assert!(!mixed.matches(&owed_quiet) && mixed.matches(&owed_need));
        assert!(mixed.matches(&mine_ready) && mixed.matches(&mine_need));
        // An archived review you owe is archived as well as quiet.
        let archived = with_statuses(owed_quiet.clone(), &["nothing-to-do", "archived"]);
        let f = query("status=archived").filter;
        assert!(f.matches(&archived) && !f.matches(&owed_quiet));
        assert_eq!(
            not_applied(&ready, Side::Owed, "me").as_deref(),
            Some("not applied here: Ready")
        );
        assert_eq!(not_applied(&ready, Side::Mine, "me"), None);
    }

    #[test]
    fn a_value_counts_and_shows_per_list() {
        let rows = [
            row(Side::Owed, Some("alice"), vec!["carol"]),
            with_statuses(row(Side::Mine, None, vec!["dave"]), &["ready"]),
            row(Side::Mine, None, vec!["carol"]),
        ];
        let f = query("author=alice&status=ready").filter;
        let reviewers = choices(&f, Facet::Reviewer, &rows, "me");
        let got: Vec<_> = reviewers
            .iter()
            .map(|c| (c.value.as_str(), c.on_keys(), c.sides.map(|s| s.rows)))
            .collect();
        // Ready leaves your PRs by dave, not carol's; alice leaves the review.
        assert_eq!(
            got,
            [
                ("carol", "owed mine".into(), [1, 0]),
                ("dave", "mine".into(), [0, 1])
            ]
        );
        let statuses = choices(&f, Facet::Status, &rows, "me");
        let ready = statuses.iter().find(|c| c.value == "ready").unwrap();
        assert_eq!(ready.on_keys(), "mine");
        assert!(
            option(Facet::Status, ready)
                .into_string()
                .contains("· your PRs")
        );
        let authors = choices(&f, Facet::Author, &rows, "me");
        assert_eq!(authors[0].on_keys(), "owed");
    }

    #[test]
    fn a_pick_a_list_lacks_but_applies_to_is_zero_there_not_elsewhere() {
        let rows = [
            row(Side::Owed, Some("a"), vec!["alice"]),
            row(Side::Mine, None, vec![]),
        ];
        let f = query("reviewer=alice").filter;
        let alice = &choices(&f, Facet::Reviewer, &rows, "me")[0];
        // Not listed on your PRs' tab, but picked it empties them: no tag.
        assert_eq!(alice.on_keys(), "owed");
        let html = option(Facet::Reviewer, alice).into_string();
        assert!(
            html.contains(r#"data-on="owed" data-applies="owed mine" data-zero="mine""#),
            "{html}"
        );
        assert!(!html.contains("ftag"));
        assert_eq!(not_applied(&f, Side::Mine, "me"), None);
    }

    #[test]
    fn a_status_that_isnt_one_filters_neither_list() {
        let f = query("status=bogus").filter;
        assert!(f.matches(&row(Side::Owed, Some("a"), vec![])));
        assert!(f.matches(&row(Side::Mine, None, vec![])));
        assert_eq!(
            not_applied(&f, Side::Mine, "me").as_deref(),
            Some("not applied here: bogus")
        );
    }

    #[test]
    fn ticking_every_reviewer_leaves_out_prs_with_none() {
        let reviewed = row(Side::Owed, Some("alice"), vec!["carol"]);
        let unreviewed = row(Side::Owed, Some("bob"), vec![]);
        let q = query("").with_facet(Facet::Reviewer, &["carol", "CAROL"]);
        assert_eq!(q.href(), "/?reviewer=carol");
        assert!(q.filter.matches(&reviewed) && !q.filter.matches(&unreviewed));
    }

    #[test]
    fn a_sent_back_href_stays_on_the_index() {
        for href in [
            "https://elsewhere.example/?author=x",
            "//elsewhere.example/",
            "?author=x",
        ] {
            assert_eq!(IndexQuery::from_href(Some(href)).href(), "/", "{href}");
        }
        assert_eq!(
            IndexQuery::from_href(Some("/?author=x&nope=1")).href(),
            "/?author=x"
        );
    }

    #[test]
    fn values_are_either_or_and_facets_all_apply() {
        let alice = row(Side::Owed, Some("alice"), vec!["carol"]);
        let bob = row(Side::Owed, Some("bob"), vec![]);
        let f = query("author=ALICE&author=bob").filter;
        assert!(f.matches(&alice) && f.matches(&bob));
        let f = query("author=alice&reviewer=dave").filter;
        assert!(!f.matches(&alice));
        let f = query("reviewer=carol&q=RETRY+%237").filter;
        assert!(f.matches(&alice) && !f.matches(&bob));
        assert!(!query("q=nope").filter.matches(&alice));
    }

    #[test]
    fn author_leaves_your_prs_alone() {
        let yours = row(Side::Mine, None, vec!["carol"]);
        assert!(query("author=alice").filter.matches(&yours));
        assert!(!query("author=alice&reviewer=dave").filter.matches(&yours));
    }

    #[test]
    fn a_facets_counts_leave_out_its_own_picks() {
        let rows = [
            row(Side::Owed, Some("alice"), vec![]),
            row(Side::Owed, Some("bob"), vec!["alice"]),
            row(Side::Mine, None, vec!["alice"]),
        ];
        let f = query("author=alice").filter;
        let authors = choices(&f, Facet::Author, &rows, "me");
        let got: Vec<_> = authors
            .iter()
            .map(|c| (c.words.as_str(), c.rows, c.picked))
            .collect();
        assert_eq!(got, [("@alice", 1, true), ("@bob", 1, false)]);
        // Only alice's PR is left among those you owe, and none of yours
        // is filtered by author.
        let reviewers = choices(&f, Facet::Reviewer, &rows, "me");
        assert_eq!(reviewers[0].rows, 1);
        let picked_gone = choices(&query("repo=org/gone").filter, Facet::Repo, &rows, "me");
        let got: Vec<_> = picked_gone
            .iter()
            .map(|c| (c.value.as_str(), c.rows))
            .collect();
        assert_eq!(got, [("org/repo", 3), ("org/gone", 0)]);
    }
}
