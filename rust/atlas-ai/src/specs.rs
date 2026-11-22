//! The ATLAS tool schemas, ported from `server/ai.py::TOOLS`.
//!
//! Every parameter carries a description and bounds, and every object is closed
//! (`additionalProperties: false`) so a small model cannot invent arguments. The
//! *handlers* for these tools live elsewhere (the database, the graph, the
//! browser); this module is the contract the model reads and
//! [`crate::tool::coerce_args`] enforces.
//!
//! The descriptions are copied, not rewritten: they are what the model was tuned
//! on, and they are what the coercion bounds mean.

use serde_json::{json, Map, Value};

use crate::tool::ToolSpec;

/// `ai.py::_obj` — one closed JSON-Schema object.
fn obj(props: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let mut map = Map::new();
    for (name, value) in props {
        map.insert(name.to_string(), value);
    }
    json!({
        "type": "object",
        "properties": Value::Object(map),
        "required": required.iter().map(|r| Value::String((*r).to_string())).collect::<Vec<Value>>(),
        "additionalProperties": false,
    })
}

/// `ai.py::_num` — a bounded integer, with an optional default.
fn num(desc: &str, default: Option<i64>, lo: i64, hi: i64) -> Value {
    let mut value = json!({
        "type": "integer",
        "description": desc,
        "minimum": lo,
        "maximum": hi,
    });
    if let Some(default) = default {
        value["default"] = json!(default);
    }
    value
}

/// `ai.py::_str`.
fn text(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}

/// `ai.py::_tags` — an array of short strings.
fn tags(desc: &str) -> Value {
    json!({ "type": "array", "description": desc, "items": { "type": "string" } })
}

/// The kinds of thing a name can be. This is a **vocabulary**, not a word filter: it says
/// what an entity is, never what an article may be about, and the model picks one per name
/// it read. `atlas-ingest` maps `Country`/`City` to the graph's `LOC` and stores the rest
/// as given, so a node in the graph carries the model's own decision.
pub const ENTITY_TYPES: [&str; 8] = [
    "Person",
    "Organization",
    "Institution",
    "Country",
    "City",
    "Event",
    "Product",
    "Other",
];

/// `save_article`'s `entities`: the named things the article names, each with what it is.
///
/// An array of objects rather than of strings because the *type* is the model's answer, and
/// a string cannot carry two answers. `atlas-ai`'s coercion keeps the objects (it cleans
/// each field and drops anything undeclared); `atlas-ingest` refuses a type outside
/// [`ENTITY_TYPES`] by leaving the name untyped, never by inventing a kind.
fn entities(desc: &str) -> Value {
    json!({
        "type": "array",
        "description": desc,
        "maxItems": 14,
        "items": {
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "type": { "type": "string", "enum": ENTITY_TYPES },
            },
            "required": ["name", "type"],
            "additionalProperties": false,
        },
    })
}

/// Every tool the agent loop may be given, in the order `ai.py` declared them.
///
/// The registry a caller builds decides which of these the model can actually
/// call; a schema without a handler still lets the model ask, and the answer is
/// the honest `unknown tool: …` the loop feeds back.
pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::new(
            "search_news",
            "Full-text search across the articles already stored in the local news database (FTS5).",
            obj(
                vec![
                    ("query", text("search terms, 2-5 words")),
                    ("hours", num("how far back to look, in hours", Some(168), 1, 720)),
                    ("limit", num("maximum number of hits", Some(15), 1, 50)),
                ],
                &["query"],
            ),
        ),
        ToolSpec::new(
            "get_article",
            "One stored article in full: text, entities, topics, thread and related articles.",
            obj(
                vec![("id", num("article id, as shown in [Article #ID]", None, 1, 100_000_000))],
                &["id"],
            ),
        ),
        ToolSpec::new(
            "related_articles",
            "Articles connected to this one through shared entities, with the reason for the link.",
            obj(
                vec![
                    ("id", num("article id", None, 1, 100_000_000)),
                    ("limit", num("maximum number of connected articles", Some(10), 1, 30)),
                ],
                &["id"],
            ),
        ),
        ToolSpec::new(
            "entity_profile",
            "Profile of an entity (person, organization, place, topic): category, mentions, newest articles, graph neighbours.",
            obj(
                vec![("name", text("entity name as it appears in the reporting"))],
                &["name"],
            ),
        ),
        ToolSpec::new(
            "graph_neighbors",
            "Subgraph around an entity (breadth-first) with edge weights and labels.",
            obj(
                vec![
                    ("entity", text("entity name")),
                    ("depth", num("how many hops out of the entity", Some(2), 1, 3)),
                    ("limit", num("maximum number of nodes", Some(80), 5, 200)),
                ],
                &["entity"],
            ),
        ),
        ToolSpec::new(
            "graph_path",
            "Shortest connection path between two entities in the graph.",
            obj(
                vec![
                    ("from_entity", text("start entity")),
                    ("to_entity", text("target entity")),
                ],
                &["from_entity", "to_entity"],
            ),
        ),
        ToolSpec::new(
            "timeline",
            "Article volume over time, optionally only for one entity.",
            obj(
                vec![
                    ("entity", text("entity name, empty for all reporting")),
                    ("days", num("how many days back", Some(7), 1, 60)),
                    ("limit", num("maximum number of articles returned", Some(200), 10, 1000)),
                ],
                &[],
            ),
        ),
        ToolSpec::new(
            "top_entities",
            "Most mentioned entities of the last N hours.",
            obj(
                vec![
                    ("hours", num("window in hours", Some(72), 1, 720)),
                    ("limit", num("maximum number of entities", Some(25), 1, 100)),
                ],
                &[],
            ),
        ),
        ToolSpec::new(
            "list_topics",
            "The reader's collection topics (what this app collects) with article counts.",
            obj(vec![], &[]),
        ),
        ToolSpec::new(
            "add_topic",
            "Create a collection topic. Only matching articles are collected for it from now on.",
            obj(
                vec![
                    ("name", text("short topic name")),
                    ("keywords", text("comma separated keywords used for matching")),
                    ("min_matches", num("how many keywords must match", Some(1), 1, 6)),
                ],
                &["name", "keywords"],
            ),
        ),
        ToolSpec::new(
            "list_categories",
            "All categories (people, organizations, ...) with entity and article counts.",
            obj(vec![], &[]),
        ),
        ToolSpec::new(
            "articles_by_category",
            "Articles in one category, e.g. People, Organizations, Events.",
            obj(
                vec![
                    ("category", text("category name")),
                    ("hours", num("how far back to look, in hours", Some(168), 1, 720)),
                    ("limit", num("maximum number of articles", Some(20), 1, 50)),
                ],
                &["category"],
            ),
        ),
        ToolSpec::new(
            "stats",
            "Database overview: articles, entities, edges, threads, sources, last collection run.",
            obj(vec![], &[]),
        ),
        ToolSpec::new(
            "web_search",
            "Search the news web in the local browser (Wikipedia, forums, social, shops filtered out). \
             Returns titles, URLs and snippets - open one with read_article or web_open.",
            obj(
                vec![
                    ("query", text("search terms, 2-5 words")),
                    ("max_results", num("maximum number of results", Some(6), 1, 20)),
                    (
                        "news_only",
                        json!({ "type": "boolean", "description": "keep only news pages", "default": true }),
                    ),
                ],
                &["query"],
            ),
        ),
        ToolSpec::new(
            "web_open",
            "Open a page in the local browser and extract the main text.",
            obj(
                vec![
                    ("url", text("http or https URL")),
                    ("max_chars", num("maximum characters of text", Some(6000), 500, 20_000)),
                ],
                &["url"],
            ),
        ),
        ToolSpec::new(
            "summarize",
            "AI summary of all stored articles matching a topic.",
            obj(
                vec![
                    ("query", text("topic or search terms")),
                    ("hours", num("how far back to look, in hours", Some(72), 1, 720)),
                ],
                &["query"],
            ),
        ),
        ToolSpec::new(
            "list_monitors",
            "All monitors with their status and last run.",
            obj(vec![], &[]),
        ),
        ToolSpec::new(
            "monitor_alerts",
            "Recent alerts from the monitors, optionally filtered to one monitor.",
            obj(
                vec![
                    ("monitor", text("monitor name, empty for all")),
                    ("hours", num("window in hours", Some(24), 1, 720)),
                    ("limit", num("maximum number of alerts", Some(20), 1, 100)),
                ],
                &[],
            ),
        ),
        ToolSpec::new(
            "find_news",
            "Search the live news web for current reporting about a query. Returns links only - \
             open the best 2-4 with read_article, then save_article what you read.",
            obj(
                vec![
                    ("query", text("search terms, 2-5 words")),
                    ("hours", num("only results from the last N hours", Some(48), 1, 72)),
                    ("limit", num("maximum number of links", Some(8), 1, 20)),
                ],
                &["query"],
            ),
        ),
        ToolSpec::new(
            "read_article",
            "Open one URL in the local browser and read the whole article text. The text is \
             cached server-side, so save_article can use it without you repeating it.",
            obj(
                vec![
                    ("url", text("http or https URL of the article")),
                    ("title", text("headline, if the search result had one")),
                ],
                &["url"],
            ),
        ),
        ToolSpec::new(
            "save_article",
            "Store an article you have actually read, with your own summary, tags, categories and \
             entities. `entities` is how the graph and the map know who and where the story is \
             about, so send it every time: the people, organizations, institutions, countries and \
             cities the page names, each with the type you decided. This writes it into the local \
             database and links it to the graph.",
            obj(
                vec![
                    ("url", text("http or https URL you read")),
                    ("title", text("headline of the article")),
                    (
                        "text",
                        text("optional: the full text, only if you have it - otherwise the cached read is used"),
                    ),
                    ("summary", text("2-3 sentences in English, facts only")),
                    ("tags", tags("4-6 short lowercase tags, reused across articles")),
                    (
                        "categories",
                        tags("e.g. Person, Organization, Institution, Country, Market, Technology, Conflict"),
                    ),
                    (
                        "entities",
                        entities(
                            "the people, organizations, institutions, countries, cities, events and \
                             products this article names, up to 8, each with the type you decided - \
                             only names you actually read in it",
                        ),
                    ),
                    ("topic", text("one of the reader's topics")),
                    ("published", text("publication date, ISO if known")),
                    ("publisher", text("outlet name")),
                    ("place", text("where it happened, only if the text says so")),
                ],
                &["url", "title", "summary", "topic"],
            ),
        ),
        ToolSpec::new(
            "tag_article",
            "Add or correct tags on an article that is already stored.",
            obj(
                vec![
                    ("id", num("article id", None, 1, 100_000_000)),
                    ("tags", tags("the corrected tag set")),
                    ("categories", tags("optional category names")),
                ],
                &["id", "tags"],
            ),
        ),
        ToolSpec::new(
            "list_tags",
            "The tags currently used in the database, most frequent first.",
            obj(vec![("limit", num("maximum number of tags", Some(30), 1, 100))], &[]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_twenty_three_tools_and_no_duplicates() {
        let tools = tools();
        assert_eq!(tools.len(), 23);
        let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        names.sort_unstable();
        let unique = {
            let mut copy = names.clone();
            copy.dedup();
            copy.len()
        };
        assert_eq!(names.len(), unique, "a duplicate tool name would shadow a handler");
    }

    #[test]
    fn every_schema_is_closed_and_described() {
        for spec in tools() {
            assert!(!spec.description.is_empty(), "{}", spec.name);
            assert_eq!(
                spec.parameters["type"], json!("object"),
                "{} must take an object",
                spec.name
            );
            assert_eq!(
                spec.parameters["additionalProperties"],
                json!(false),
                "{} must be closed",
                spec.name
            );
            for (name, property) in spec.properties() {
                assert!(
                    property.get("description").is_some() || name == "news_only",
                    "{}.{} has no description",
                    spec.name,
                    name
                );
                let kind = property["type"].as_str().unwrap_or("");
                assert!(
                    ["integer", "string", "array", "boolean"].contains(&kind),
                    "{}.{} has an unhandled type {:?}",
                    spec.name,
                    name,
                    kind
                );
                if kind == "integer" {
                    assert!(property.get("minimum").is_some(), "{}.{} has no minimum", spec.name, name);
                    assert!(property.get("maximum").is_some(), "{}.{} has no maximum", spec.name, name);
                }
            }
            for required in spec.required() {
                assert!(
                    spec.properties().contains_key(&required),
                    "{} requires {:?}, which it does not declare",
                    spec.name,
                    required
                );
            }
        }
    }

    #[test]
    fn bounds_match_the_python_table() {
        let tools = tools();
        let search = tools.iter().find(|t| t.name == "search_news").unwrap();
        let props = search.properties();
        assert_eq!(props["hours"]["default"], json!(168));
        assert_eq!(props["hours"]["maximum"], json!(720));
        assert_eq!(props["limit"]["default"], json!(15));
        assert_eq!(props["limit"]["minimum"], json!(1));

        let save = tools.iter().find(|t| t.name == "save_article").unwrap();
        assert_eq!(
            save.required(),
            vec!["url", "title", "summary", "topic"],
            "the required set decides what the model must supply"
        );

        let web = tools.iter().find(|t| t.name == "web_search").unwrap();
        assert_eq!(web.properties()["news_only"]["default"], json!(true));
    }

    /// The graph's types come from the model, so the schema has to ask for them in the one
    /// shape that can carry a name *and* a kind. A string array would lose the kind.
    #[test]
    fn save_article_asks_the_model_for_names_and_their_kinds() {
        let tools = tools();
        let save = tools.iter().find(|t| t.name == "save_article").unwrap();
        let entities = &save.properties()["entities"];
        assert_eq!(entities["type"], json!("array"));
        assert_eq!(entities["items"]["type"], json!("object"));
        assert_eq!(entities["items"]["properties"]["name"]["type"], json!("string"));
        let kinds = entities["items"]["properties"]["type"]["enum"]
            .as_array()
            .expect("the kind is an enum, not free text");
        let spelled: Vec<&str> = kinds.iter().filter_map(|k| k.as_str()).collect();
        assert_eq!(spelled, ENTITY_TYPES.to_vec(), "the schema and the crate agree");
        assert_eq!(entities["items"]["additionalProperties"], json!(false));
        assert_eq!(
            entities["items"]["required"],
            json!(["name", "type"]),
            "a name without a kind is the half-answer this field exists to end"
        );
        // And it is optional: a page that names nobody must still be storable.
        assert!(
            !save.required().iter().any(|name| name == "entities"),
            "entities must stay optional, or a story naming nobody could not be saved"
        );
    }
}
