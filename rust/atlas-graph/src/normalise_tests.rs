//! The identity pass: an alias pair merges and keeps its edges, a second run changes
//! nothing, a rebuilt weight equals the articles that mention both ends, and a reason
//! written by the model survives the rebuild that recomputes the weights.

use std::path::PathBuf;

use atlas_store::{Store, SqlValue};

use crate::graphdb::{recompute_edges, Caches, Context};
use crate::normalise::{normalise, plan};

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}

fn int(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}

fn db(label: &str) -> (PathBuf, Store) {
    let path = std::env::temp_dir()
        .join(format!("atlas-normalise-{}-{}", std::process::id(), label))
        .join("atlas.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let store = Store::open(&path).expect("open");
    store.init().expect("init");
    (path, store)
}

fn article(store: &Store, id: i64, title: &str) {
    let published = atlas_store::iso_ago(2.0);
    store
        .exec(
            "INSERT INTO articles(id,title,publisher,source_id,published,summary,content,url, \
             relevant,dupe,full_text,iso,place,lat,lon) \
             VALUES(?1,?2,'t',1,?3,'','',?4,1,0,1,'DEU','Berlin',52.5,13.4)",
            &[
                int(id),
                text(title),
                text(&published),
                text(&format!("https://example.invalid/{id}")),
            ],
        )
        .expect("an article");
}

fn entity(store: &Store, id: i64, name: &str, kind: &str, count: i64) {
    store
        .exec(
            "INSERT INTO entities(id,name,type,count,first_seen,last_seen) VALUES(?1,?2,?3,?4,?5,?5)",
            &[
                int(id),
                text(name),
                text(kind),
                int(count),
                text(&atlas_store::iso_ago(3.0)),
            ],
        )
        .expect("an entity");
}

fn mentions(store: &Store, article_id: i64, entity_id: i64) {
    store
        .exec(
            "INSERT OR IGNORE INTO article_entities(article_id,entity_id,weight) VALUES(?1,?2,1.0)",
            &[int(article_id), int(entity_id)],
        )
        .expect("a mention");
}

fn edges(store: &Store) -> Vec<(i64, i64, i64)> {
    let mut out: Vec<(i64, i64, i64)> = store
        .query("SELECT a,b,weight FROM edges ORDER BY a,b", &[])
        .expect("edges")
        .iter()
        .map(|row| {
            (
                row.int("a").unwrap_or(0),
                row.int("b").unwrap_or(0),
                row.int("weight").unwrap_or(0),
            )
        })
        .collect();
    out.sort_unstable();
    out
}

fn names(store: &Store) -> Vec<String> {
    let mut out: Vec<String> = store
        .query("SELECT name FROM entities ORDER BY name", &[])
        .expect("entities")
        .iter()
        .map(|row| row.text("name").unwrap_or("").to_string())
        .collect();
    out.sort();
    out
}

/// `Trump` / `Donald` / `Donald Trump`, `Jensen Huang` / `Jensen`: the archive's own
/// split identities, each mentioned in articles of its own.
fn split_identities(store: &Store) {
    for (id, name, kind, count) in [
        (1, "Donald Trump", "PERSON", 40),
        (2, "Trump", "ORG", 90),
        (3, "Donald", "ORG", 20),
        (4, "Jensen Huang", "PERSON", 30),
        (5, "Jensen", "ORG", 12),
        (6, "China", "LOC", 200),
    ] {
        entity(store, id, name, kind, count);
    }
    // Each form is used alone in its own article, which is the evidence that they are
    // one identity; `Hunter` and `Biden` below are the counter-example.
    article(store, 1, "Trump signs an order");
    mentions(store, 1, 2);
    mentions(store, 1, 6);
    article(store, 2, "Donald Trump meets Xi");
    mentions(store, 2, 1);
    mentions(store, 2, 6);
    article(store, 3, "Donald speaks at the fair");
    mentions(store, 3, 3);
    article(store, 4, "Jensen Huang at the launch");
    mentions(store, 4, 4);
    article(store, 5, "Jensen on the record");
    mentions(store, 5, 5);
    article(store, 6, "China and the chip rules");
    mentions(store, 6, 6);
    // Every form is also linked to a second article: a merge rests on article evidence,
    // and a name the archive linked once is a headline word it never used again.
    for (article_id, entity_id) in [(7, 1), (8, 2), (9, 3), (10, 4), (11, 5)] {
        article(store, article_id, "a second mention");
        mentions(store, article_id, entity_id);
    }
}

#[test]
fn an_alias_pair_merges_and_keeps_its_edges() {
    let (_path, store) = db("alias");
    split_identities(&store);
    let before = names(&store);
    assert_eq!(before.len(), 6);

    let report = normalise(&store, 30, false).expect("the pass");
    assert!(!report.dry_run);
    assert_eq!(report.entities_before, 6);
    // 2 -> 1, 3 -> 1, 5 -> 4: three rows folded, two identities left besides China
    assert_eq!(report.merges.len(), 3, "{:?}", report.merges);
    let merged: Vec<(String, String)> = report
        .merges
        .iter()
        .map(|merge| (merge.from_name.clone(), merge.into_name.clone()))
        .collect();
    assert!(merged.contains(&("Trump".to_string(), "Donald Trump".to_string())));
    assert!(merged.contains(&("Donald".to_string(), "Donald Trump".to_string())));
    assert!(merged.contains(&("Jensen".to_string(), "Jensen Huang".to_string())));
    assert_eq!(
        names(&store),
        vec!["China".to_string(), "Donald Trump".to_string(), "Jensen Huang".to_string()],
        "the fuller name survives"
    );

    // the relations did not disappear with the rows: every article that named a folded
    // form now points at the survivor, and the edges are rebuilt from those mentions
    assert_eq!(
        edges(&store),
        vec![(1, 6, 2), (1, 4, 0)].into_iter().filter(|edge| edge.2 > 0).collect::<Vec<_>>(),
        "Donald Trump and China share two articles; Jensen Huang shares none with either"
    );
    let trump = store
        .query("SELECT count(*) n FROM article_entities WHERE entity_id=1", &[])
        .expect("links")[0]
        .int("n")
        .unwrap_or(0);
    assert_eq!(
        trump, 6,
        "Trump, Donald Trump and Donald are one identity in six articles once folded"
    );
    // and the alias rows remember what was folded, so a later article naming "Trump"
    // resolves to the survivor
    let aliases = store
        .query("SELECT alias FROM entity_alias ORDER BY alias", &[])
        .expect("aliases");
    assert_eq!(aliases.len() >= 3, true, "Trump, Donald and Jensen are aliases now");
}

#[test]
fn a_second_run_changes_nothing() {
    let (_path, store) = db("idempotent");
    split_identities(&store);
    let first = normalise(&store, 30, false).expect("the first pass");
    assert_eq!(first.merges.len(), 3);
    let after_first = (names(&store), edges(&store));

    let second = normalise(&store, 30, false).expect("the second pass");
    assert_eq!(second.merges.len(), 0, "the folded rows are gone, nothing is left to fold");
    assert_eq!(second.entities_before, second.entities_after);
    assert_eq!(second.edges_before, second.edges_after);
    assert_eq!(second.weights_changed, 0, "the weights were already the evidence");
    assert_eq!((names(&store), edges(&store)), after_first, "the second run wrote nothing");
}

#[test]
fn a_rebuilt_weight_is_the_articles_that_mention_both_ends() {
    let (_path, store) = db("weights");
    entity(&store, 1, "Nvidia", "ORG", 10);
    entity(&store, 2, "China", "LOC", 10);
    for id in [1, 2, 3] {
        article(&store, id, "Nvidia and China again");
        mentions(&store, id, 1);
        mentions(&store, id, 2);
    }
    // a relation written by hand with a weight no article supports
    store
        .exec("INSERT INTO edges(a,b,weight,type) VALUES(1,2,9,'cooccur')", &[])
        .expect("a stale edge");

    let report = normalise(&store, 30, false).expect("the pass");
    assert_eq!(report.weights_changed, 1, "the wrong weight is counted as repaired");
    assert_eq!(edges(&store), vec![(1, 2, 3)], "three articles mention both ends");
}

#[test]
fn a_reason_survives_the_rebuild_that_recomputes_the_weights() {
    let (_path, store) = db("labels");
    entity(&store, 1, "Nvidia", "ORG", 10);
    entity(&store, 2, "China", "LOC", 10);
    for id in [1, 2] {
        article(&store, id, "Nvidia and China");
        mentions(&store, id, 1);
        mentions(&store, id, 2);
    }
    store
        .exec(
            "INSERT INTO edges(a,b,weight,type,label) VALUES(1,2,2,'cooccur','supplies chips to')",
            &[],
        )
        .expect("a labelled edge");

    // the rebuild that used to run `DELETE FROM edges` and re-insert (a,b,weight) only
    let mut caches = Caches::new();
    let done = recompute_edges(&store, &mut caches, &Context::default(), 30).expect("recompute");
    assert_eq!(done.labelled, 1, "the reason is carried over the rebuild");
    let label = store
        .query("SELECT label FROM edges WHERE a=1 AND b=2", &[])
        .expect("edge")[0]
        .text("label")
        .unwrap_or("")
        .to_string();
    assert_eq!(label, "supplies chips to", "the model's sentence is still on the relation");
}

#[test]
fn two_people_named_in_one_article_are_never_merged() {
    let (_path, store) = db("conamed");
    entity(&store, 1, "Biden", "PERSON", 20);
    entity(&store, 2, "Hunter Biden", "PERSON", 12);
    // the evidence that says they are two people: one story names both
    article(&store, 1, "Hunter Biden and Biden at the same table");
    mentions(&store, 1, 1);
    mentions(&store, 1, 2);

    assert!(plan(&store).expect("plan").is_empty(), "co-mention is the counter-evidence");
    let report = normalise(&store, 30, false).expect("the pass");
    assert_eq!(report.merges.len(), 0);
    assert_eq!(names(&store).len(), 2);
}

#[test]
fn a_lowercase_fragment_is_never_a_candidate() {
    let (_path, store) = db("fragment");
    entity(&store, 1, "Nvidia", "ORG", 20);
    entity(&store, 2, "nvidia", "ORG", 4);
    article(&store, 1, "Nvidia and nvidia");
    mentions(&store, 1, 1);
    article(&store, 2, "nvidia again");
    mentions(&store, 2, 2);
    // the same word in two casings is not two identities: the case-insensitive key of
    // the table already refuses the duplicate name, so this pair cannot even be stored.
    assert!(plan(&store).expect("plan").is_empty(), "one word is one row");
}

#[test]
fn a_dry_run_plans_and_writes_nothing() {
    let (_path, store) = db("dry");
    split_identities(&store);
    let before = (names(&store), edges(&store));

    let report = normalise(&store, 30, true).expect("the dry run");
    assert!(report.dry_run);
    assert_eq!(report.merges.len(), 3, "it plans the same three folds");
    assert_eq!(report.entities_after, 3);
    assert!(report.edges_after > 0, "the merged identity still has its relations");
    assert_eq!((names(&store), edges(&store)), before, "the archive is untouched");

    // and the real pass afterwards does exactly what the dry run said
    let real = normalise(&store, 30, false).expect("the pass");
    assert_eq!(real.merges.len(), report.merges.len());
    assert_eq!(real.entities_after, report.entities_after);
    assert_eq!(real.edges_after, report.edges_after);
    assert_eq!(real.weights_changed, report.weights_changed);
}

#[test]
fn a_declared_alias_merges_the_row_it_names() {
    let (_path, store) = db("declared");
    // The archive keeps the model's own declaration for a name it once wrote
    // differently: `APEC Summit` was declared to mean `Apec Summit`, and both rows
    // survived. No word of either name is a word of the other in a different order,
    // so the shape rule alone cannot see it — the declaration can.
    entity(&store, 1, "Apec Summit", "ORG", 6);
    entity(&store, 2, "APEC Summit", "ORG", 5);
    entity(&store, 3, "China", "LOC", 40);
    store
        .exec(
            "INSERT INTO entity_alias(alias,entity_id) VALUES(?1,?2)",
            &[text("APEC Summit"), int(1)],
        )
        .expect("a declared alias");
    article(&store, 1, "The APEC summit opens");
    mentions(&store, 1, 1);
    mentions(&store, 1, 3);
    article(&store, 2, "Apec summit closes");
    mentions(&store, 2, 2);
    mentions(&store, 2, 3);

    let report = normalise(&store, 30, false).expect("the pass");
    assert_eq!(report.merges.len(), 1, "{:?}", report.merges);
    assert_eq!(report.merges[0].from_name, "APEC Summit");
    assert_eq!(report.merges[0].into_name, "Apec Summit");
    assert_eq!(names(&store), vec!["Apec Summit", "China"]);
    // the folded row's relations and mentions are kept, not dropped
    assert_eq!(edges(&store), vec![(1, 3, 2)]);
    assert!(plan(&store).expect("plan").is_empty(), "and only once");
}

#[test]
fn a_word_the_prose_lowercases_is_not_a_name() {
    let (_path, store) = db("prose");
    // Two rows the shape rules would join and two that they would not: `Three` is a word
    // the sentence lowercases, `Riemann` is not.
    entity(&store, 1, "Three", "ORG", 6);
    entity(&store, 2, "Three French", "PERSON", 6);
    entity(&store, 3, "Riemann", "PERSON", 6);
    entity(&store, 4, "Riemann Hypothesis", "PERSON", 6);
    // Each row keeps its own articles, so co-mention is not what decides here.
    for (article_id, text, entity_id) in [
        (1, "three of the three are here", 1),
        (2, "there were three of them", 1),
        (3, "Three French writers arrived", 2),
        (4, "Three French writers left", 2),
        (5, "The Riemann hypothesis, Riemann wrote", 3),
        (6, "Riemann on primes", 3),
        (7, "Riemann Hypothesis, again Riemann", 4),
        (8, "Riemann Hypothesis stated plainly", 4),
    ] {
        article(&store, article_id, text);
        mentions(&store, article_id, entity_id);
    }

    let merges = plan(&store).expect("plan");
    assert_eq!(merges.len(), 1, "{:?}", merges);
    assert_eq!(merges[0].from_name, "Riemann");
    assert_eq!(merges[0].into_name, "Riemann Hypothesis");
}
