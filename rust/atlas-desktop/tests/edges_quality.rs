//! The relation table, read back through the two queries the graph page runs.
//!
//! The report was "relations look wrong or missing", so these tests pin what the page
//! asks the archive for rather than what it paints: a node has to be able to carry a
//! line, a line needs both of its ends drawn, and the heaviest relation comes first. The
//! defect that produced the report was in the *node* query — 84 of the 120 nodes it
//! returned had no relation at all, so the canvas was two thirds dots — and the numbers
//! behind that are in `data.rs::entities`'s own doc.
//!
//! Two kinds of test live here: exact assertions against a throwaway archive whose rows
//! this file writes (so the rules are pinned without the checkout's data), and one read
//! of the real archive in `data/`, which is skipped with a printed reason when there is
//! none — a fresh clone has no archive, and a test that failed there would teach nobody
//! anything.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use atlas_desktop::data::Archive;

/// A window wide enough to include any stamp these fixtures carry, so the tests never
/// depend on the clock: the window filter itself is pinned in `data/tests.rs`.
const ANY_WINDOW: i64 = 24 * 365 * 10;

/// A throwaway directory holding one archive, removed when the test ends.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    /// A fresh archive with the tables and the two indexes the graph queries use — the
    /// shipped index set, so the plan these tests exercise is the plan the window runs.
    fn new(rows: &str) -> (Scratch, Archive) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "atlas-edges-quality-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let file = dir.join("atlas.db");
        let conn = rusqlite::Connection::open(&file).expect("a fresh archive");
        conn.execute_batch(
            "CREATE TABLE entities(
                 id INTEGER PRIMARY KEY, name TEXT, type TEXT, count INTEGER, last_seen TEXT);
             CREATE INDEX idx_entities_count ON entities(count DESC);
             CREATE TABLE edges(
                 a INTEGER, b INTEGER, weight REAL DEFAULT 1, type TEXT DEFAULT 'cooccur',
                 label TEXT, PRIMARY KEY(a, b, type));
             CREATE INDEX idx_edges_b ON edges(b);",
        )
        .expect("the schema data.rs reads");
        conn.execute_batch(rows).expect("the fixture's rows");
        drop(conn);
        let archive = Archive::open(&file).expect("the archive opens read-only");
        (Scratch { dir }, archive)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn ids(entities: &[atlas_desktop::data::Entity]) -> Vec<i64> {
    entities.iter().map(|entity| entity.id).collect()
}

#[test]
fn a_node_is_only_drawn_when_a_relation_can_be_drawn_from_it() {
    // The defect this pins: the nodes were the most-mentioned entities, whether or not
    // the archive had a single relation for them. On the real archive that was 84 of the
    // 120 nodes, which is what "the relations are missing" looked like.
    let (_scratch, archive) = Scratch::new(
        "INSERT INTO entities(id,name,type,count,last_seen) VALUES
             (1,'China','LOC',261,'2026-01-01T00:00:00Z'),
             (2,'Trump','ORG',149,'2026-01-01T00:00:00Z'),
             (3,'Italia','ORG',200,'2026-01-01T00:00:00Z'),
             (4,'Nvidia','ORG',170,'2026-01-01T00:00:00Z');
         -- 2 is only ever the second end, 3 is named by nothing at all
         INSERT INTO edges(a,b,weight) VALUES (1,2,9.0), (4,2,3.0);",
    );
    let drawn = archive.entities(ANY_WINDOW, 120);
    assert_eq!(
        ids(&drawn),
        vec![1, 4, 2],
        "the three entities a relation touches, strongest mention first; Italia (200 \
         mentions, no relation) does not take a slot"
    );
    // and the limit still bounds the answer, not the filter
    assert_eq!(ids(&archive.entities(ANY_WINDOW, 2)), vec![1, 4]);
}

#[test]
fn a_line_is_drawn_only_between_two_nodes_that_are_drawn() {
    let (_scratch, archive) = Scratch::new(
        "INSERT INTO entities(id,name,type,count,last_seen) VALUES
             (1,'China','LOC',261,'2026-01-01T00:00:00Z'),
             (2,'Trump','ORG',149,'2026-01-01T00:00:00Z'),
             (3,'Nvidia','ORG',170,'2026-01-01T00:00:00Z');
         -- the heaviest relation in the table, stored with its ends the other way round
         INSERT INTO edges(a,b,weight) VALUES (1,2,4.0), (3,2,9.0);",
    );
    let both = archive.edges(&[1, 2], 900);
    assert_eq!(both.len(), 1, "only the relation whose two ends are both drawn");
    assert_eq!((both[0].a, both[0].b, both[0].weight), (1, 2, 4.0));
    // the 9.0 is stored as (3,2): it must be found when 3 is drawn and not before, which
    // is what the `+a IN … AND b IN …` shape has to keep true
    let reversed = archive.edges(&[2, 3], 900);
    assert_eq!(reversed.len(), 1);
    assert_eq!((reversed[0].a, reversed[0].b, reversed[0].weight), (3, 2, 9.0));
    assert!(
        archive.edges(&[], 900).is_empty(),
        "an empty node set is no lines, not a syntax error"
    );
}

#[test]
fn the_heaviest_relations_come_first_and_the_limit_is_a_ceiling() {
    let (_scratch, archive) = Scratch::new(
        "INSERT INTO entities(id,name,type,count,last_seen) VALUES
             (1,'A','ORG',9,'2026-01-01T00:00:00Z'),
             (2,'B','ORG',8,'2026-01-01T00:00:00Z'),
             (3,'C','ORG',7,'2026-01-01T00:00:00Z');
         INSERT INTO edges(a,b,weight) VALUES (1,2,1.0), (1,3,5.0), (2,3,3.0);",
    );
    let all = archive.edges(&[1, 2, 3], 900);
    let weights: Vec<f64> = all.iter().map(|edge| edge.weight).collect();
    assert_eq!(weights, vec![5.0, 3.0, 1.0], "the order the picture draws in");
    assert_eq!(
        archive.edges(&[1, 2, 3], 2).len(),
        2,
        "the limit is a ceiling on lines, not on relations"
    );
}

#[test]
fn a_row_keeps_its_weight_and_a_blank_reason_stays_blank() {
    let (_scratch, archive) = Scratch::new(
        "INSERT INTO entities(id,name,type,count,last_seen) VALUES
             (1,'A','ORG',9,'2026-01-01T00:00:00Z'),
             (2,'B','ORG',8,'2026-01-01T00:00:00Z'),
             (3,'C','ORG',7,'2026-01-01T00:00:00Z');
         INSERT INTO edges(a,b,weight,label) VALUES
             (1,2,NULL,NULL), (1,3,4.0,'governs');",
    );
    let mut rows = archive.edges(&[1, 2, 3], 900);
    rows.sort_by(|left, right| left.b.cmp(&right.b));
    assert_eq!(rows[0].weight, 1.0, "a relation with no weight is one story");
    assert_eq!(rows[0].label, "", "no reason is read as no reason");
    assert_eq!(rows[1].label, "governs");
}

#[test]
fn the_real_archive_draws_a_connected_graph() {
    let path = atlas_desktop::assets::archive();
    let archive = match Archive::open(&path) {
        Ok(archive) => archive,
        Err(problem) => {
            println!("skipped: {problem}");
            return;
        }
    };

    // the load `app.rs` runs, in its order
    let nodes = archive.entities(72, 120);
    let node_ids = ids(&nodes);
    let lines = archive.edges(&node_ids, 900);
    if nodes.is_empty() || lines.is_empty() {
        println!("skipped: no graph in this archive yet");
        return;
    }

    for edge in &lines {
        assert!(
            node_ids.contains(&edge.a) && node_ids.contains(&edge.b),
            "a line points at a node that is not drawn: {} -> {}",
            edge.a,
            edge.b
        );
        assert!(edge.weight > 0.0, "a relation of zero stories is not a relation");
        assert_ne!(edge.a, edge.b, "an entity related to itself is not a line");
    }
    let mut pairs: Vec<(i64, i64)> = lines
        .iter()
        .map(|edge| (edge.a.min(edge.b), edge.a.max(edge.b)))
        .collect();
    pairs.sort_unstable();
    let before = pairs.len();
    pairs.dedup();
    assert_eq!(before, pairs.len(), "the same pair came back twice");

    // Every drawn node should carry a line. Before the node query required a relation
    // this was 36 of 120; the assertion is loose enough to survive a thinner window and
    // tight enough to fail if the mention ranking comes back without its filter.
    let connected: Vec<i64> = node_ids
        .iter()
        .copied()
        .filter(|id| lines.iter().any(|edge| edge.a == *id || edge.b == *id))
        .collect();
    let share = 100.0 * connected.len() as f64 / node_ids.len() as f64;
    println!(
        "archive: {} nodes, {} lines, {} connected ({share:.0}%)",
        node_ids.len(),
        lines.len(),
        connected.len()
    );
    assert!(
        share >= 80.0,
        "only {share:.0}% of the drawn nodes carry a relation ({} of {})",
        connected.len(),
        node_ids.len()
    );
    assert!(
        lines.len() >= 300,
        "{} lines for {} nodes is the shape the missing-relations report described",
        lines.len(),
        node_ids.len()
    );
}
