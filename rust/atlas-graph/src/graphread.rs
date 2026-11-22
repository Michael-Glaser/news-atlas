//! The read side the UI asks for — `bfs_subgraph()`, `top_subgraph()`,
//! `graph_path()`, `links_for()`, and the two entity lists `ai.py` publishes
//! (`t_entity_profile`, `t_top_entities`).
//!
//! Every statement is the Python one. Where `server/graph.py` relies on SQLite's
//! table order for a tie it has no second key for (`ORDER BY count DESC`,
//! `ORDER BY ar.published DESC`), the port adds the id as the last key: the rows
//! are the same rows, and the answer is the same twice in a row. Nothing here
//! reads or writes anything but the database it is told to open.

use atlas_lib::json::Json;
use atlas_store::{row_to_json_text, rows_to_json_text, Row, SqlValue, Store, StoreError};

use crate::graphdb::{
    fail, int, jarr, jfixed, jnum, jobj, jstr, need_id, need_text, open, opt_f64, opt_i64,
    opt_usize, real, value_text,
};
use crate::Lex;

/// What [`graph_path`] found.
pub struct PathFound {
    /// Entity names from `a` to `b`, or empty when there is no connection.
    pub path: Vec<String>,
    pub found: bool,
}

/// `bfs_subgraph(entity_id, depth=2, limit=150)` — the subgraph around one
/// entity, strongest edges first.
///
/// Rows come back in the order the Python loop sees them (`ORDER BY weight DESC`,
/// ties left to SQLite), and the walk stops at `limit` nodes just as it does
/// there. The nodes are then filtered — an entity whose name is page chrome is
/// not drawn — and only edges between two surviving nodes are kept.
pub fn bfs_subgraph(
    store: &Store,
    entity_id: i64,
    depth: usize,
    limit: usize,
) -> Result<String, StoreError> {
    let mut nodes: Vec<i64> = vec![entity_id];
    let mut level: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    level.insert(entity_id, 0);
    let mut frontier: Vec<i64> = vec![entity_id];
    let mut edges: Vec<(i64, i64, SqlValue, SqlValue)> = Vec::new();
    for d in 0..depth {
        if frontier.is_empty() {
            break;
        }
        let marks = atlas_store::placeholders(frontier.len());
        let mut params: Vec<SqlValue> = frontier.iter().map(|id| int(*id)).collect();
        params.extend(frontier.iter().map(|id| int(*id)));
        let rows = store.query(
            &format!(
                "SELECT a,b,weight,label FROM edges WHERE a IN ({}) OR b IN ({}) \
                 ORDER BY weight DESC LIMIT 400",
                marks, marks
            ),
            &params,
        )?;
        let mut next: Vec<i64> = Vec::new();
        for row in &rows {
            let a = row.int("a").unwrap_or(0);
            let b = row.int("b").unwrap_or(0);
            edges.push((
                a,
                b,
                row.get("weight").cloned().unwrap_or(SqlValue::Null),
                row.get("label").cloned().unwrap_or(SqlValue::Null),
            ));
            for node in [a, b] {
                if !level.contains_key(&node) {
                    level.insert(node, d + 1);
                    nodes.push(node);
                    next.push(node);
                    if nodes.len() >= limit {
                        break;
                    }
                }
            }
            if nodes.len() >= limit {
                break;
            }
        }
        frontier = next;
    }
    let marks = atlas_store::placeholders(nodes.len());
    let params: Vec<SqlValue> = nodes.iter().map(|id| int(*id)).collect();
    let lex = Lex::new();
    let mut ents: Vec<Row> = store.query(
        &format!(
            "SELECT id,name,type,count FROM entities WHERE id IN ({}) ORDER BY id",
            marks
        ),
        &params,
    )?;
    ents.retain(|row| {
        row.int("id") == Some(entity_id) || !lex.is_junk_name(row.text("name").unwrap_or(""))
    });
    let seen: std::collections::HashSet<i64> =
        ents.iter().map(|row| row.int("id").unwrap_or(0)).collect();
    let mut kept: Vec<String> = Vec::new();
    for (a, b, weight, label) in &edges {
        if !seen.contains(a) || !seen.contains(b) {
            continue;
        }
        kept.push(jobj(&[
            ("source", jnum(*a as f64)),
            ("target", jnum(*b as f64)),
            ("weight", value_text(weight)),
            ("label", value_text(label)),
        ]));
    }
    Ok(jobj(&[
        ("mode", jstr("bfs")),
        ("center", jnum(entity_id as f64)),
        ("nodes", rows_to_json_text(&ents)),
        ("edges", jarr(&kept)),
        ("depth", jnum(depth as f64)),
    ]))
}

/// `top_subgraph(limit=40, min_weight=2)` — the global connection network:
/// strongest entities and the edges between them.
pub fn top_subgraph(store: &Store, limit: usize, min_weight: f64) -> Result<String, StoreError> {
    let lex = Lex::new();
    let limit = std::cmp::max(1, limit);
    let rows = store.query(
        "SELECT id,name,type,count FROM entities ORDER BY count DESC, id LIMIT ?",
        &[int(std::cmp::max(limit * 3, limit) as i64)],
    )?;
    let mut tops: Vec<Row> = Vec::new();
    for row in rows {
        if lex.is_junk_name(row.text("name").unwrap_or("")) {
            continue;
        }
        tops.push(row);
        if tops.len() >= limit {
            break;
        }
    }
    if tops.is_empty() {
        return Ok(jobj(&[
            ("mode", jstr("top")),
            ("nodes", jarr(&[])),
            ("edges", jarr(&[])),
            ("center", "null".to_string()),
        ]));
    }
    let ids: Vec<i64> = tops.iter().map(|row| row.int("id").unwrap_or(0)).collect();
    let marks = atlas_store::placeholders(ids.len());
    let mut params: Vec<SqlValue> = ids.iter().map(|id| int(*id)).collect();
    params.extend(ids.iter().map(|id| int(*id)));
    params.push(real(min_weight));
    let edges = store.query(
        &format!(
            "SELECT a,b,weight,label FROM edges WHERE a IN ({}) AND b IN ({}) \
             AND weight>=? ORDER BY weight DESC LIMIT 400",
            marks, marks
        ),
        &params,
    )?;
    let mut linked: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for row in &edges {
        linked.insert(row.int("a").unwrap_or(0));
        linked.insert(row.int("b").unwrap_or(0));
    }
    let mut nodes: Vec<Row> = tops
        .iter()
        .filter(|row| linked.contains(&row.int("id").unwrap_or(0)))
        .cloned()
        .collect();
    if nodes.is_empty() {
        nodes = tops.clone();
    }
    let keep: std::collections::HashSet<i64> = nodes
        .iter()
        .map(|row| row.int("id").unwrap_or(0))
        .collect();
    let mut kept: Vec<String> = Vec::new();
    for row in &edges {
        let a = row.int("a").unwrap_or(0);
        let b = row.int("b").unwrap_or(0);
        if !keep.contains(&a) || !keep.contains(&b) {
            continue;
        }
        kept.push(jobj(&[
            ("source", jnum(a as f64)),
            ("target", jnum(b as f64)),
            (
                "weight",
                value_text(&row.get("weight").cloned().unwrap_or(SqlValue::Null)),
            ),
            (
                "label",
                value_text(&row.get("label").cloned().unwrap_or(SqlValue::Null)),
            ),
        ]));
    }
    Ok(jobj(&[
        ("mode", jstr("top")),
        ("nodes", rows_to_json_text(&nodes)),
        ("edges", jarr(&kept)),
        ("center", "null".to_string()),
    ]))
}

/// `graph_path(a_name, b_name, max_depth=6)` — the shortest connection path
/// between two entities.
///
/// `max_depth` is accepted and unused, exactly as in `server/graph.py`: the Python
/// walk is bounded by the "more than 5 000 visited" guard, not by the argument, so
/// honouring the argument here would answer differently from the module the bridge
/// falls back to.
pub fn graph_path(store: &Store, a_name: &str, b_name: &str) -> Result<PathFound, StoreError> {
    let a = store.query_one("SELECT id,name FROM entities WHERE name=?", &[atlas_store_s(text_of(a_name))])?;
    let b = store.query_one("SELECT id,name FROM entities WHERE name=?", &[atlas_store_s(text_of(b_name))])?;
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            return Ok(PathFound {
                path: Vec::new(),
                found: false,
            })
        }
    };
    let start = a.int("id").unwrap_or(0);
    let goal = b.int("id").unwrap_or(0);
    if start == goal {
        return Ok(PathFound {
            path: vec![a.text("name").unwrap_or("").to_string()],
            found: true,
        });
    }
    let mut prev: std::collections::HashMap<i64, Option<i64>> = std::collections::HashMap::new();
    prev.insert(start, None);
    let mut queue: std::collections::VecDeque<i64> = std::collections::VecDeque::new();
    queue.push_back(start);
    let mut found = false;
    while let Some(current) = queue.pop_front() {
        if found {
            break;
        }
        if prev.len() > 5000 {
            break;
        }
        let rows = store.query(
            "SELECT a,b FROM edges WHERE a=? OR b=? ORDER BY weight DESC",
            &[int(current), int(current)],
        )?;
        for row in &rows {
            let a = row.int("a").unwrap_or(0);
            let b = row.int("b").unwrap_or(0);
            let next = if a == current { b } else { a };
            if prev.contains_key(&next) {
                continue;
            }
            prev.insert(next, Some(current));
            if next == goal {
                found = true;
                break;
            }
            queue.push_back(next);
        }
    }
    if !prev.contains_key(&goal) {
        return Ok(PathFound {
            path: Vec::new(),
            found: false,
        });
    }
    let mut path: Vec<String> = Vec::new();
    let mut node = Some(goal);
    while let Some(current) = node {
        let name = store
            .query_one("SELECT name FROM entities WHERE id=?", &[int(current)])?
            .and_then(|row| row.text("name").map(|s| s.to_string()))
            .unwrap_or_else(|| current.to_string());
        path.push(name);
        node = prev.get(&current).copied().flatten();
    }
    path.reverse();
    Ok(PathFound { path, found: true })
}

fn text_of(value: &str) -> &str {
    value
}

fn atlas_store_s(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}

/// `links_for(article_id, limit=10)` — the connections of one article, strongest
/// first, including the `why`.
pub fn links_for(store: &Store, article_id: i64, limit: i64) -> Result<Vec<Row>, StoreError> {
    store.query(
        "SELECT a2.id,a2.title,a2.published,a2.iso,a2.url,l.shared,l.score,l.why,s.name AS source \
         FROM article_links l JOIN articles a2 ON a2.id=CASE WHEN l.a=? THEN l.b ELSE l.a END \
         LEFT JOIN sources s ON s.id=a2.source_id WHERE (l.a=? OR l.b=?) AND a2.relevant=1 \
         ORDER BY l.score DESC, a2.id LIMIT ?",
        &[int(article_id), int(article_id), int(article_id), int(limit)],
    )
}

/// `_find_entity()` — exact name, then an alias, then the closest name.
fn find_entity(store: &Store, name: &str) -> Result<Option<Row>, StoreError> {
    let value = || SqlValue::Text(name.to_string());
    if let Some(row) = store.query_one("SELECT * FROM entities WHERE name=?", &[value()])? {
        return Ok(Some(row));
    }
    if let Some(alias) = store.query_one(
        "SELECT entity_id FROM entity_alias WHERE alias=? COLLATE NOCASE",
        &[value()],
    )? {
        if let Some(row) = store.query_one(
            "SELECT * FROM entities WHERE id=?",
            &[int(alias.int("entity_id").unwrap_or(0))],
        )? {
            return Ok(Some(row));
        }
    }
    store.query_one(
        "SELECT * FROM entities WHERE name LIKE ? ORDER BY count DESC, id LIMIT 1",
        &[SqlValue::Text(format!("%{}%", name))],
    )
}

/// `ai.t_entity_profile()`'s graph part: the entity, its category, its newest
/// articles, its neighbours (with the ids the UI needs to open a connection) and
/// its aliases.
pub fn entity_profile(store: &Store, name: &str) -> Result<Option<String>, StoreError> {
    let entity = match find_entity(store, name)? {
        Some(row) => row,
        None => return Ok(None),
    };
    let id = entity.int("id").unwrap_or(0);
    let category = match entity.int("category_id") {
        Some(cid) => store
            .query_one("SELECT name FROM categories WHERE id=?", &[int(cid)])?
            .and_then(|row| row.text("name").map(|s| s.to_string())),
        None => None,
    };
    let articles = store.query(
        "SELECT ar.id,ar.title,ar.published,ar.iso,s.name AS source FROM article_entities ae \
         JOIN articles ar ON ar.id=ae.article_id LEFT JOIN sources s ON s.id=ar.source_id \
         WHERE ae.entity_id=? AND ar.relevant=1 ORDER BY ar.published DESC, ar.id DESC LIMIT 12",
        &[int(id)],
    )?;
    let neighbors = store.query(
        "SELECT e2.id,e2.name,e2.type,e2.count,ed.weight,ed.label FROM edges ed \
         JOIN entities e2 ON e2.id=CASE WHEN ed.a=? THEN ed.b ELSE ed.a END \
         WHERE ed.a=? OR ed.b=? ORDER BY ed.weight DESC, e2.id LIMIT 25",
        &[int(id), int(id), int(id)],
    )?;
    let aliases = store.query(
        "SELECT alias FROM entity_alias WHERE entity_id=? ORDER BY alias",
        &[int(id)],
    )?;
    let alias_list: Vec<String> = aliases
        .iter()
        .filter_map(|row| row.text("alias"))
        .map(jstr)
        .collect();
    Ok(Some(jobj(&[
        ("found", "true".to_string()),
        ("entity", row_to_json_text(&entity)),
        (
            "category",
            match category {
                Some(name) => jstr(&name),
                None => "null".to_string(),
            },
        ),
        ("articles", rows_to_json_text(&articles)),
        ("neighbors", rows_to_json_text(&neighbors)),
        ("aliases", jarr(&alias_list)),
    ])))
}

/// `ai.t_top_entities()` — the entities of the last `hours`, most-covered first.
pub fn top_entities(store: &Store, hours: i64, limit: i64) -> Result<Vec<Row>, StoreError> {
    store.query(
        "SELECT e.name,e.type,e.count,COUNT(DISTINCT ae.article_id) AS recent FROM entities e \
         JOIN article_entities ae ON ae.entity_id=e.id JOIN articles ar ON ar.id=ae.article_id \
         WHERE ar.relevant=1 AND ar.published>=? GROUP BY e.id \
         ORDER BY recent DESC, e.count DESC, e.id LIMIT ?",
        &[
            // The cutoff in the archive's own timestamp format: a `datetime('now', ?)`
            // window comes back as `2026-11-05 10:00:00` and `T` sorts above the
            // space, so the window ran a day wide (see `graphdb::window_ago`).
            SqlValue::Text(atlas_store::clock::iso_ago(hours as f64)),
            int(limit),
        ],
    )
}

// ---------------------------------------------------------------- commands

/// `subgraph` — one entity's neighbourhood (`entity_id`), or the global network
/// (`limit`, `min_weight`).
pub fn cmd_subgraph(input: &Json) -> Result<String, String> {
    let store = open(input).map_err(fail)?;
    let bfs = input.get("entity_id").is_some();
    let limit = opt_usize(input, "limit", if bfs { 150 } else { 40 });
    match input.get("entity_id").and_then(|v| v.as_f64()) {
        Some(entity_id) => {
            let depth = std::cmp::max(1, std::cmp::min(opt_usize(input, "depth", 2), 3));
            bfs_subgraph(&store, entity_id as i64, depth, limit).map_err(fail)
        }
        None => {
            let min_weight = opt_f64(input, "min_weight", 2.0);
            top_subgraph(&store, limit, min_weight).map_err(fail)
        }
    }
}

/// `path` — the shortest connection between two entity names.
pub fn cmd_path(input: &Json) -> Result<String, String> {
    let store = open(input).map_err(fail)?;
    let from = need_text(input, "from").map_err(fail)?;
    let to = need_text(input, "to").map_err(fail)?;
    let found = graph_path(&store, &from, &to).map_err(fail)?;
    let names: Vec<String> = found.path.iter().map(|name| jstr(name)).collect();
    Ok(jobj(&[
        ("found", if found.found { "true" } else { "false" }.to_string()),
        ("path", jarr(&names)),
        (
            "length",
            jnum(if found.path.is_empty() {
                0.0
            } else {
                (found.path.len() - 1) as f64
            }),
        ),
        ("source", jstr("rust")),
    ]))
}

/// `links_for` — `{"db":…,"id":…,"limit":…}`.
pub fn cmd_links_for(input: &Json) -> Result<String, String> {
    let store = open(input).map_err(fail)?;
    let article_id = need_id(input, "id").map_err(fail)?;
    let limit = std::cmp::max(1, opt_i64(input, "limit", 10));
    let rows = links_for(&store, article_id, limit).map_err(fail)?;
    Ok(jobj(&[
        ("related", rows_to_json_text(&rows)),
        ("count", jnum(rows.len() as f64)),
        ("source", jstr("rust")),
    ]))
}

/// `profile` — `{"db":…,"name":…}`.
pub fn cmd_profile(input: &Json) -> Result<String, String> {
    let store = open(input).map_err(fail)?;
    let name = need_text(input, "name").map_err(fail)?;
    match entity_profile(&store, &name).map_err(fail)? {
        Some(body) => Ok(body),
        None => Ok(jobj(&[
            ("found", "false".to_string()),
            ("error", jstr("entity not found")),
        ])),
    }
}

/// `top_entities` — `{"db":…,"hours":72,"limit":25}`.
pub fn cmd_top_entities(input: &Json) -> Result<String, String> {
    let store = open(input).map_err(fail)?;
    let hours = std::cmp::max(1, std::cmp::min(opt_i64(input, "hours", 72), 720));
    let limit = std::cmp::max(1, std::cmp::min(opt_i64(input, "limit", 25), 50));
    let rows = top_entities(&store, hours, limit).map_err(fail)?;
    Ok(jobj(&[
        ("entities", rows_to_json_text(&rows)),
        ("count", jnum(rows.len() as f64)),
        ("source", jstr("rust")),
    ]))
}

// `jfixed` is used by the recompute answer, not here; the import keeps the
// module's helper set in one place.
#[allow(dead_code)]
fn decimals() -> &'static str {
    "ms"
}

#[allow(dead_code)]
fn unused_marker(value: f64) -> String {
    jfixed(value, 1)
}
