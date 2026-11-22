//! What a disagreement is, and what fixing it costs.
//!
//! Decisions: the closed list of things this crate can find ([`Kind`]), which of them
//! make `verify` fail ([`Severity`]), and how a fix travels — a [`Batch`] of one
//! parameterised statement plus the values for each row it touches, so the statement
//! can be *listed* before it is run.
//!
//! It does not: find anything (the checks do), run anything ([`crate::normalize`]
//! does), or hold article text — a finding carries ids, counts and a reason.

use atlas_store::SqlValue;

/// One kind of disagreement.
///
/// The id is what the report and the JSON print, so it stays stable when the code
/// moves: a run book can key on `ref.article_tags` for years.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An article with no `article_fts` entry: search cannot find it.
    FtsMissing,
    /// An index entry whose article is gone.
    FtsOrphan,
    /// The index does not hold the text the article now has.
    FtsStale,
    /// `ORDER BY rank` on the index fails or answers nothing.
    FtsRank,
    /// `snippet()` on the index fails: the search UI reads it.
    FtsSnippet,
    /// A child table pointing at an article that is not there.
    DanglingArticle,
    /// `article_entities` or `edges` pointing at an entity that is not there.
    DanglingEntity,
    /// `article_topics` pointing at a topic that is not there.
    DanglingTopic,
    /// `article_categories` pointing at a category that is not there.
    DanglingCategory,
    /// `article_threads` pointing at a thread that is not there.
    DanglingThread,
    /// `entity_alias` pointing at an entity that is not there.
    DanglingAlias,
    /// Two spellings of one outlet, one of them a variant of the other.
    PublisherVariant,
    /// An article with no publisher at all, which `verify` can fill from its delivery host.
    PublisherEmpty,
    /// A stored place that occurs nowhere in the article's own text.
    PlaceUnsupported,
    /// An unsupported place where the place engine names no alternative.
    PlaceUnnamed,
    /// A tag that cannot be a label (markup, punctuation, too long).
    TagInvalid,
    /// A tag that repeats another tag on the same article.
    TagVariant,
    /// A tag that only repeats the outlet's name.
    TagPublisher,
    /// `dupe=1` with `relevant=1`, which the data model says never happens.
    RelevantDupe,
    /// `edges.a >= edges.b`, which the data model says never happens.
    EdgeOrder,
    /// An edge from an entity to itself.
    EdgeSelf,
    /// `article_links.a >= article_links.b`.
    LinkOrder,
    /// An article with no URL: unfixable here, the URL is not in the archive.
    ArticleWithoutUrl,
    /// An article with no headline.
    ArticleWithoutTitle,
    /// An entity nothing links to.
    EntityOrphan,
    /// An entity the model has not classified (the documented "untyped" state).
    EntityUntyped,
    /// A tag whose provenance cannot be told apart from the model's own tags.
    TagProvenance,
}

impl Kind {
    /// The stable id the report prints.
    pub fn id(self) -> &'static str {
        match self {
            Kind::FtsMissing => "fts.missing",
            Kind::FtsOrphan => "fts.orphan",
            Kind::FtsStale => "fts.stale",
            Kind::FtsRank => "fts.rank",
            Kind::FtsSnippet => "fts.snippet",
            Kind::DanglingArticle => "ref.articles",
            Kind::DanglingEntity => "ref.entities",
            Kind::DanglingTopic => "ref.topics",
            Kind::DanglingCategory => "ref.categories",
            Kind::DanglingThread => "ref.threads",
            Kind::DanglingAlias => "ref.aliases",
            Kind::PublisherVariant => "publisher.variant",
            Kind::PublisherEmpty => "publisher.empty",
            Kind::PlaceUnsupported => "place.unsupported",
            Kind::PlaceUnnamed => "place.unnamed",
            Kind::TagInvalid => "tag.invalid",
            Kind::TagVariant => "tag.variant",
            Kind::TagPublisher => "tag.publisher",
            Kind::RelevantDupe => "relevant.dupe",
            Kind::EdgeOrder => "edge.order",
            Kind::EdgeSelf => "edge.self",
            Kind::LinkOrder => "link.order",
            Kind::ArticleWithoutUrl => "article.url",
            Kind::ArticleWithoutTitle => "article.title",
            Kind::EntityOrphan => "entity.orphan",
            Kind::EntityUntyped => "entity.untyped",
            Kind::TagProvenance => "tag.provenance",
        }
    }

    /// Does this make `verify` fail? A `Broken` finding is a disagreement with the
    /// schema or the model; a `Note` is work the carry-over still has to do, and
    /// reporting it as a failure would make `verify` fail on a healthy archive
    /// forever.
    pub fn severity(self) -> Severity {
        match self {
            Kind::EntityOrphan | Kind::EntityUntyped | Kind::TagProvenance | Kind::PlaceUnnamed => {
                Severity::Note
            }
            _ => Severity::Broken,
        }
    }

    /// What the finding is, in one line, without any value from the archive.
    pub fn reason(self) -> &'static str {
        match self {
            Kind::FtsMissing => "articles with no full-text index entry",
            Kind::FtsOrphan => "full-text entries whose article is gone",
            Kind::FtsStale => "indexed text that no longer matches the article",
            Kind::FtsRank => "ORDER BY rank does not answer on the index",
            Kind::FtsSnippet => "snippet() does not answer on the index",
            Kind::DanglingArticle => "rows pointing at an article that does not exist",
            Kind::DanglingEntity => "rows pointing at an entity that does not exist",
            Kind::DanglingTopic => "rows pointing at a topic that does not exist",
            Kind::DanglingCategory => "rows pointing at a category that does not exist",
            Kind::DanglingThread => "rows pointing at a thread that does not exist",
            Kind::DanglingAlias => "aliases pointing at an entity that does not exist",
            Kind::PublisherVariant => "one outlet stored under two spellings",
            Kind::PublisherEmpty => "articles with no publisher stored",
            Kind::PlaceUnsupported => "a stored place with no evidence in the text",
            Kind::PlaceUnnamed => "a stored place the text does not support and the engine cannot replace",
            Kind::TagInvalid => "tags that cannot be a label",
            Kind::TagVariant => "tags repeating another tag on the same article",
            Kind::TagPublisher => "tags that only repeat the outlet",
            Kind::RelevantDupe => "hidden duplicates still marked relevant",
            Kind::EdgeOrder => "edges stored with a > b",
            Kind::EdgeSelf => "edges from an entity to itself",
            Kind::LinkOrder => "article links stored with a > b",
            Kind::ArticleWithoutUrl => "articles with no URL",
            Kind::ArticleWithoutTitle => "articles with no headline",
            Kind::EntityOrphan => "entities no article links",
            Kind::EntityUntyped => "entities the model has not classified",
            Kind::TagProvenance => "tags whose provenance cannot be told apart",
        }
    }
}

/// Whether a finding fails `verify`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Broken,
    Note,
}

/// One parameterised statement and the values for every row it touches.
///
/// `params.len()` is the row count the fix will change; the statement itself is
/// printed once. This is what "list every statement before running it" means here.
#[derive(Clone, Debug)]
pub struct Batch {
    pub kind: Kind,
    /// The table the statement touches — schema, so it may be printed.
    pub subject: &'static str,
    pub sql: &'static str,
    pub params: Vec<Vec<SqlValue>>,
    /// Rows a statement that selects its own rows (a `… NOT IN (SELECT …)`) will touch.
    /// A parameterised statement does not need it: its row count *is* `params.len()`.
    pub planned: usize,
    /// What a reader has to know to price the statement ("one statement, re-derives all
    /// 1 435 documents"), in schema-and-count words only. `to_text` prints it under the
    /// statement, because a row count alone understates a rebuild.
    pub note: String,
}

impl Batch {
    pub fn new(kind: Kind, subject: &'static str, sql: &'static str) -> Batch {
        Batch {
            kind,
            subject,
            sql,
            params: Vec::new(),
            planned: 0,
            note: String::new(),
        }
    }

    /// Say what the statement really does, when its row count cannot.
    pub fn with_note(mut self, note: impl Into<String>) -> Batch {
        self.note = note.into();
        self
    }

    pub fn push(&mut self, row: Vec<SqlValue>) {
        self.params.push(row);
    }

    /// A statement whose `WHERE` clause is a subquery: the check already counted how
    /// many rows disagree, and the plan prints that number instead of pretending the fix
    /// binds it.
    pub fn plan_rows(&mut self, rows: usize) {
        self.planned = rows;
    }

    /// How many rows this statement will touch.
    pub fn rows(&self) -> usize {
        if self.planned > 0 {
            self.planned
        } else {
            self.params.len()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.planned == 0 && self.params.is_empty()
    }
}

/// One thing that disagrees.
#[derive(Clone, Debug)]
pub struct Finding {
    pub kind: Kind,
    /// Which table (or which check) — schema names only.
    pub subject: String,
    /// How many rows disagree.
    pub count: usize,
    /// The first ids, for a human to look at. Ids and keys only, never text.
    pub ids: Vec<String>,
    /// A reason built from counts and schema, never from a value.
    pub detail: String,
    /// The statements that fix it, when a fix exists in the archive itself.
    pub batch: Option<Batch>,
}

impl Finding {
    pub fn new(kind: Kind, subject: impl Into<String>, count: usize) -> Finding {
        Finding {
            kind,
            subject: subject.into(),
            count,
            ids: Vec::new(),
            detail: String::new(),
            batch: None,
        }
    }

    pub fn with_ids(mut self, ids: Vec<String>) -> Finding {
        self.ids = ids;
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Finding {
        self.detail = detail.into();
        self
    }

    pub fn with_batch(mut self, batch: Batch) -> Finding {
        self.batch = Some(batch);
        self
    }

    pub fn is_broken(&self) -> bool {
        self.kind.severity() == Severity::Broken
    }

    pub fn fixable(&self) -> bool {
        self.batch.is_some()
    }

    /// `id: N row(s) — reason — detail`, the line a report prints.
    pub fn line(&self) -> String {
        let mut out = format!(
            "{}  {}  {} row{}",
            self.kind.id(),
            marker(self.kind.severity()),
            self.count,
            if self.count == 1 { "" } else { "s" }
        );
        if !self.subject.is_empty() {
            out.push_str("  ");
            out.push_str(&self.subject);
        }
        out.push_str(" — ");
        out.push_str(self.kind.reason());
        if !self.detail.is_empty() {
            out.push_str(" (");
            out.push_str(&self.detail);
            out.push(')');
        }
        out
    }
}

/// `!` for a broken finding, `-` for a note: monochrome, no colour, and a marker
/// that survives a pipe into a file.
pub fn marker(severity: Severity) -> &'static str {
    match severity {
        Severity::Broken => "!",
        Severity::Note => "-",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_stable_unique_id() {
        let kinds = [
            Kind::FtsMissing,
            Kind::FtsOrphan,
            Kind::FtsStale,
            Kind::FtsRank,
            Kind::FtsSnippet,
            Kind::DanglingArticle,
            Kind::DanglingEntity,
            Kind::DanglingTopic,
            Kind::DanglingCategory,
            Kind::DanglingThread,
            Kind::DanglingAlias,
            Kind::PublisherVariant,
            Kind::PublisherEmpty,
            Kind::PlaceUnsupported,
            Kind::PlaceUnnamed,
            Kind::TagInvalid,
            Kind::TagVariant,
            Kind::TagPublisher,
            Kind::RelevantDupe,
            Kind::EdgeOrder,
            Kind::EdgeSelf,
            Kind::LinkOrder,
            Kind::ArticleWithoutUrl,
            Kind::ArticleWithoutTitle,
            Kind::EntityOrphan,
            Kind::EntityUntyped,
            Kind::TagProvenance,
        ];
        let mut ids: Vec<&str> = kinds.iter().map(|kind| kind.id()).collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique, "two kinds share an id");
        for kind in kinds {
            assert!(!kind.reason().is_empty());
            assert!(kind.id().contains('.'));
        }
    }

    #[test]
    fn coverage_gaps_do_not_fail_verify() {
        assert_eq!(Kind::EntityUntyped.severity(), Severity::Note);
        assert_eq!(Kind::TagProvenance.severity(), Severity::Note);
        assert_eq!(Kind::DanglingArticle.severity(), Severity::Broken);
    }

    #[test]
    fn a_finding_line_names_no_value() {
        let finding = Finding::new(Kind::DanglingArticle, "article_tags", 3)
            .with_detail("3 rows")
            .with_ids(vec!["12".to_string()]);
        assert_eq!(
            finding.line(),
            "ref.articles  !  3 rows  article_tags — rows pointing at an article that does not exist (3 rows)"
        );
    }

    #[test]
    fn a_batch_counts_its_rows() {
        let mut batch = Batch::new(Kind::PublisherVariant, "articles", "UPDATE articles SET publisher=? WHERE id=?");
        assert!(batch.is_empty());
        batch.push(vec![SqlValue::Text("Folha".to_string()), SqlValue::Integer(7)]);
        assert_eq!(batch.rows(), 1);
        assert!(batch.note.is_empty(), "a parameterised statement needs no note");
    }

    #[test]
    fn an_empty_publisher_is_its_own_kind_and_not_a_variant() {
        // A fill and a variant merge are two different repairs; one reason line cannot be
        // true of both, so they carry two ids.
        assert_ne!(Kind::PublisherEmpty.id(), Kind::PublisherVariant.id());
        assert_eq!(Kind::PublisherEmpty.severity(), Severity::Broken);
        assert!(Kind::PublisherEmpty.reason().contains("no publisher"));
        assert!(!Kind::PublisherEmpty.reason().contains("two spellings"));
    }
}
