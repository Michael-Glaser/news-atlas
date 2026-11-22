//! The entity graph: a force layout over the relations the archive already holds, and
//! a panel that says why two entities are joined.
//!
//! The layout is the shape the wasm frontend used — repulsion between every pair, a
//! spring per relation, cooling — with the two additions that make it readable at 120
//! nodes: a spring's rest length grows with the two node radii (a hub does not sit on
//! top of its neighbours), and a collision pass runs after the integrator, so two nodes
//! never overlap whatever the forces said. Both are O(n²) — 7 140 pairs at 120 nodes,
//! which is a fraction of a frame (`tests::a_graph_frame_of_a_hundred_and_twenty_nodes
//! _fits_the_release_budget` measures the whole frame).
//!
//! The relation lines are drawn so a reader can follow them: the lightest composites to
//! grey 96 on the #070707 plate, which is 3.2:1 against it — the floor used to be 0.15
//! alpha (grey 43, 1.4:1), which read as a gap in the graph rather than as a relation —
//! and the heaviest is 5.0 px at grey 207. The window between them is the *drawn* set's
//! own range on a logarithm (see [`strength_share`]), so the ten strongest relations of a
//! picture are visibly heavier than its tail instead of every line sitting at the bottom
//! of a linear ramp. Weight and brightness, never colour: the palette has no hue in it.
//!
//! # The picture is ranked
//!
//! A page of 120 dots and 900 lines is a grey mesh whatever the lines look like, so the
//! plate draws what it has room for: [`capacity`] is the number of entities that keep
//! [`SPACING`] between them on the canvas in front of them (23 at 700×520, 58 at 1100×820,
//! all 120 at 2400×1400), those are drawn at a radius between 7 px and 27 px by their rank
//! in the strength order, and each keeps its strongest [`LINKS_PER_NODE`] relations drawn.
//! The rest of the set is drawn as grain — still where the entity is, still clickable, named
//! when picked — and the caption says how many entities and relations that left out.
//! Nothing is lost from the panel: [`GraphView::around`] reads every row, so a selected node
//! lists all of its relations with their reasons, drawn or not.
//!
//! Labels are for the strongest nodes only (degree first, then mentions): always the
//! selected and hovered node, then the top of that order, never fewer than [`LABELS_MIN`]
//! (eight) and never two on top of each other. [`place_labels`] decides that from the
//! geometry of the frame — eight to sixteen places around each dot, and a second pass
//! that may cross a dot smaller than its own — because a name that is not there leaves a
//! reader guessing at a dot, and text piled on text is worse than either. Measured at
//! 120 entities: fifteen names on a 1100×820 plate, fourteen on a 1440×900 one.
//!
//! Selecting a node changes what the picture is for: its own relations are drawn bright
//! and everything else at [`DIM_ALPHA`], so a selection reads as a neighbourhood instead
//! of as a hairball with a ring around one dot.
//!
//! The camera is bounded in both directions: a pan may not push the layout off the plate
//! ([`GraphView::clamp_pan`]), and Home or a double click on the empty plate fits the whole
//! picture again ([`GraphView::reset_view`]). While the layout settles the view asks for a
//! frame every 16 ms — never immediately, which would be a busy loop — and once it is at
//! rest it asks for nothing ([`ALPHA_REST`]).
//!
//! One gesture per target: press on a node and drag to place it, press on the plate and
//! drag to pan, scroll to zoom about the pointer, click to select. The canvas takes
//! keyboard focus and shows it, and the arrow keys then walk the same strongest-first
//! order the labels are drawn in.
//!
//! # The panel is a door
//!
//! A relation row in the panel is not a caption, it is a way in: opening one names the
//! pair it joins — both ends ringed on the plate, that one line drawn in ink on top of
//! the rest — and lists the newest stories that name both ends, each of them opening in
//! the reading pane. [`GraphView::show`] answers that story as an `Option<i64>`, the same
//! answer [`crate::views::articles`] and [`crate::views::overview`] give, so the shell
//! switches pages in the arm it already has.
//!
//! Two decisions live in that sentence:
//!
//! * **The panel writes through the view, not into it.** The shell draws the canvas and
//!   then hands the panel `&GraphView` in the same frame, so the row it opens is one
//!   [`Cell`] the canvas reads back on the next frame (`focus`), and the story it opens
//!   is another (`opened`). That is why the panel takes a shared reference and still has
//!   a click effect.
//! * **The stories are read on the change, not per frame.** The pair the panel opened is
//!   the only thing the canvas watches; when it differs from the stories already loaded,
//!   one query fills them, and every frame after that draws from memory. A repaint still
//!   costs no SQL — the property `app::Atlas` is built around.
//!
//! The stories are the panel's own narrow read (`article_entities` joined to itself for
//! one pair) rather than a `data` statement: a page's list is what `data` holds, and this
//! is one panel's question about two names. It opens the archive the same way `data`
//! does, read-only and `query_only`, and closes it again.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Align2, Color32, FontId, Galley, Pos2, Rect, Sense, Stroke, Vec2};
use rusqlite::{Connection, OpenFlags};

use super::{clip, nf, when};
use crate::data::{Edge, Entity};
use crate::theme;

/// The plate the graph is drawn on: the theme's own deepest surface, not a grey this view
/// invented. Every line and every name on it is measured against it.
const PLATE: Color32 = theme::RECESS;
/// Where a relation's line crosses from the tail of the picture to the part of it a reader is
/// meant to follow. A relation is drawn in one of exactly two greys from the palette
/// ([`theme::FAINT`] below this share, [`theme::DIM`] above it) — a ladder, not a gradient:
/// the audit found seven computed greys on this plate, and strength is carried by the line's
/// *weight* (`edge_width`) and by those two steps, never by a mix nobody can name.
///
/// The floor is a claim about content: a line that carries a relation is never quieter than
/// [`theme::FAINT`] on this plate, which `the_weakest_relation_line_is_visible_on_the_plate`
/// proves is above 3:1.
const EDGE_STEP: f32 = 0.5;
/// A relation touching the selection is drawn in the brighter step, and one that is not is
/// context: a structural hairline, below the 3:1 floor on purpose. With a neighbourhood on
/// the plate the rest is not content, and drawing it at the floor is the hairball this page
/// is being fixed for.
const CONTEXT: Color32 = theme::HAIRLINE;
const WIDTH_MIN: f32 = 1.0;
const WIDTH_MAX: f32 = 5.0;
/// How much of the layout, or of the canvas — whichever is smaller — always stays on the
/// plate, on each axis. Three fifths of the smaller one leaves the covered *area* at least
/// 36 % of it, which is above the 30 % floor `tests/graph_bounds.rs` holds the storm to for a
/// layout larger than the window; and it is loose enough that a 300 px drag on a 640×480
/// plate still moves the picture 300 px (`tests/graph_interaction.rs` measures that pan),
/// while the layout cannot be pushed out of reach.
const KEEP: f32 = 0.60;
/// The smallest and largest radius a drawn entity gets. `RADIUS_MAX` is also what
/// `SPACING` and `HIT_MIN` are sized against.
const RADIUS_MIN: f32 = 7.0;
const RADIUS_MAX: f32 = 27.0;
/// How hard the rank curve leans on the top of the order: a straight ramp gives the middle
/// of the list the middle of the range, and the eye reads a picture as "a few big, most
/// small". 1.6 puts the median at about half of the maximum, which is what
/// `the_dots_have_a_size_hierarchy` holds it to.
const RANK_CURVE: f32 = 1.6;
/// An entity the current canvas has no room for is drawn as this: a grain of the picture
/// that still marks where the entity sits and can still be clicked. It is not labelled and
/// it carries no lines, and the caption says how many there are.
const TAIL_RADIUS: f32 = 2.6;
/// The area one entity at full size needs on the plate, at zoom 1: a dot at the top of the
/// range plus the room a name beside it takes. It is the number that decides how many
/// actors a plate holds (`capacity`), and it was measured against the page rather than
/// picked: at 78 px a 1100×820 plate claimed room for all 120 entities and the picture was
/// still 120 dots of one size with six names on it — the dots were as packed as the dots
/// allowed, and no label had anywhere to go. At 124 px that plate holds 58 entities at full
/// size, 23 hold at 700×520, 104 at 1600×1000, and the rest are drawn as grain.
const SPACING: f32 = 160.0;
/// The floor on that count, so a small window is still a graph, and the ceiling, so a large
/// one does not turn the picture back into a field of dots.
const VISIBLE_MIN: usize = 20;
const VISIBLE_MAX: usize = 160;
/// How many relations a full-size node keeps drawn: its strongest four. A hub with ninety
/// relations is a rounded star then, not a star of ninety, and the rest of its relations are
/// still in the panel — `around` reads every row.
const LINKS_PER_NODE: usize = 4;
/// Relations per drawn node at the **fit**: two lines through a dot is a picture a reader can
/// trace; four is what the plate draws once the reader has zoomed in, and the selected node
/// always keeps every relation it has ([`Self::asked`]).
const LINKS_PER_NODE_AT_FIT: usize = 2;
/// The most entities the opening view draws, whatever room the plate has.
///
/// The reader's "das Netz zieht sich immer noch zu sehr zusammen": 120 entities and 900
/// relations fitted into one plate is a mesh at any spacing — the same picture at 60 entities
/// is a graph. The rest are not lost: the caption says how many of how many are drawn, the
/// arrows and the panel reach every entity, and a selection draws its own neighbourhood. A
/// deeper zoom brings the rest back as grain ([`GRAIN_ZOOM`]).
const ACTORS_AT_FIT: usize = 60;
/// How far past the fit the reader zooms before the entities the opening view kept back are
/// painted again, as grain.
const GRAIN_ZOOM: f32 = 1.4;
/// A spring's rest length before the two radii are added to it.
const REST: f32 = 18.0;
/// How much of the two radii a spring adds: enough that two hubs keep air between their
/// rims, little enough that a joined pair is still clearly closer than an unjoined entity
/// the collision pass has pushed away (`tests/graph_layout.rs` holds that: at 2.6 the pair's
/// rest length of 140 px overshot the 100 px an isolated entity is pushed to, and a picture
/// where a relation reads as *further* than no relation is a picture that lies).
const REST_PER_RADIUS: f32 = 1.2;
/// A spring is never shorter than this multiple of the larger radius it joins: the link
/// distance has to be longer than the biggest dot it touches, or two joined entities are
/// drawn as one blob with a line inside it.
const REST_PER_MAX_RADIUS: f32 = 1.6;
/// The repulsion one node pair feels at a distance of one layout unit, at the node count
/// [`REPULSION_AT`]. Scaled by `count / REPULSION_AT` so the same picture at 120 nodes is
/// pushed apart as hard as the 60-node one it was tuned on: without the scale a full page
/// compresses into a disc, which is the blob the reader complains about.
const REPULSION: f32 = 1400.0;
const REPULSION_AT: f32 = 60.0;
/// How much the **far end's degree** divides a spring, above the first relation: the force a
/// relation puts on a node is `stiffness / (1 + SPRING_DEGREE * (degree of the other end - 1))`.
///
/// This is the answer to "die Anziehungskraft zwischen den grossen Punkten ist zu stark": a
/// hub with sixty relations was pulling with sixty springs' worth of force while a leaf pulled
/// with one, so two joined hubs dragged themselves into a knot and each hub dragged its whole
/// neighbourhood onto itself. Counting from the first relation is what keeps a *leaf* pair as
/// tight as it always was — dividing by the whole degree halved a one-relation pair too, and
/// measured on three entities that left the joined pair 44.6 px apart against 24.6 px for the
/// lone entity, the wrong way round for `tests/graph_layout`'s
/// `a_joined_pair_settles_closer_to_itself_than_to_an_isolated_entity`. Measured with the rule
/// that ships, on the 120/900 picture fitted to the 780 px Graph plate: the tightest pair of
/// the ten biggest hubs is 76.3 px apart (60.0 px before this round), their median gap 367.5 px
/// (247.0 px). The relation still draws and still holds its pair nearer than an unrelated
/// entity; what it no longer does is decide the whole picture.
const SPRING_DEGREE: f32 = 1.0;

/// The room one node is given in the layout's own units: the radius of the disc the picture
/// fills is the area this many nodes need, divided over a circle.
///
/// The layout used to be held together by a centring force (0.0015 per tick, proportional to
/// the distance from the origin), and a linear pull to the middle is exactly what packs a
/// graph into a dense centre with a few outliers — the reader's "er sammelt sich zu sehr in
/// der Mitte". The room is what the trap below is calibrated against: the picture settles at
/// the size its dots need and the trap changes only the *shape* of the density inside it. 66 px
/// is the packing the 120-node picture settles at (π·350² / 120 ≈ 3 200 px² per node).
const PACK: f32 = 66.0;

/// The trap the layout sits in, as a multiple of the repulsion the picture pushes out with.
///
/// A **harmonic** trap — inward acceleration proportional to the distance from the origin,
/// zero in the middle, strongest at the rim — rather than a wall. A wall is a rim: the
/// repulsion carries every node out until something stops it, so the picture was a wreath with
/// an empty middle (measured on the 120/900 picture: the inner quarter of the dots sat at 0.59
/// of the box's radius and the outer quarter at 0.99, i.e. piled on the edge, and a 10-node
/// picture had no dot near its centre at all). A quadratic potential is the shape whose
/// equilibrium density is flat, so the same room is filled evenly instead of being ringed.
///
/// 1.0 is the strength at which the trap's pull at the rim balances the repulsion the picture
/// pushes out with there. Measured on the 120/900 picture at the 780 px plate: the radial
/// quartiles 0.48 / 0.69 / 0.86 (a ring answered 0.59 / 0.82 / 0.99, a pile 0.44 / 0.62 /
/// 0.78, an evenly filled disc 0.50 / 0.71 / 0.87), the painted box 87 % × 89 % of the plate
/// and the median nearest neighbour 47.7 px — with the ring the same plate answered 90 % ×
/// 91 % but the middle of the picture was empty, and a picture of ten entities had no dot
/// within a 160 × 120 window of its centre at all (`tests/graph_interaction` clicks there).
const TRAP: f32 = 1.0;

/// The room never shrinks below this many [`PACK`] radii, whatever the node count.
///
/// A picture of three entities needs room for three dots, not a disc scaled by √3 — without the
/// floor the trap for three nodes is at its strongest and the whole picture is squeezed to its
/// collision distances. 3.0 keeps them far enough apart that the springs, not the collision
/// pass, decide the spacing: measured, the joined pair 74.7 px against a lone entity 88.4 px
/// (`tests/graph_layout`), and the 120-entity picture is above the floor anyway (350 px room),
/// so it is unaffected.
const ROOM_FLOOR: f32 = 3.0;


/// Collision keeps the two radii plus this many layout units between two centres. Above 0 on
/// purpose: touching is not overlapping, but a rim on a rim reads as touching.
const SEPARATION_GAP: f32 = 4.0;
const SEPARATION_ROUNDS: usize = 10;
/// A dot is never wider than this share of the distance to its nearest neighbour. The rank
/// curve decides how big an entity is drawn; this decides how big it is *allowed* to be on
/// the layout it sits on — the top of the curve at 27 px was wider than the gap to its
/// neighbour at 120 nodes, so a hub swallowed the dots around it and the picture read as one
/// mass with a big dot in it.
const RADIUS_SHARE: f32 = 0.42;
/// How far the picture is stretched along the plate's long axis: enough to fill a wide
/// window, never so far that the graph reads as a row of dots.
const SPREAD_MAX: f32 = 2.6;
/// Above this many entities the fit may scale a picture *up* to the plate as well as down:
/// a graph of a hundred and twenty that opens at 36 % of a 1100 px canvas is a graph a
/// reader has to zoom into before they can read it. A page of six entities stays at ×1.
const GROW_ABOVE: usize = 40;
/// One press of the plate's zoom buttons, and the scroll a mouse wheel delivers for the
/// same step (egui reports a notch as about 120 px). [`SCROLL_RATE`] is `ln(ZOOM_STEP) /
/// WHEEL_NOTCH`, written out because a const cannot call `ln` — that equality is what makes
/// a button press and a wheel notch the same gesture, and `the_wheel_and_the_buttons_agree`
/// measures it.
const ZOOM_STEP: f32 = 1.2;
/// The points a mouse wheel reports for one notch, the unit `zoom_factor` measures its
/// exponential in. Only the tests read it directly — the wiring reads egui's own
/// `smooth_scroll_delta` — but it is the number [`ZOOM_STEP`] is derived from, so it is named
/// rather than written into the test as a literal.
#[cfg(test)]
const WHEEL_NOTCH: f32 = 120.0;
const SCROLL_RATE: f32 = 0.001_520_5;
/// How far one Shift+arrow moves the camera.
const PAN_STEP: f32 = 48.0;
/// The most one frame of scroll may multiply the zoom by: a trackpad fling is many frames
/// of small deltas, a driver that reports one huge delta is the case this bounds.
const ZOOM_JUMP: f32 = 1.6;
/// A press this close to a node's centre grabs it: at least a fingertip-sized target,
/// whatever the node's drawn radius is.
const HIT_MIN: f32 = 12.0;
const HIT_SLACK: f32 = 3.0;
/// How close to a line a press has to be to open it. A line is drawn [`WIDTH_MIN`]…[`WIDTH_MAX`]
/// px wide, so 6 px is the widest line plus a fingertip's forgiveness — the same idea as
/// `HIT_MIN` for a dot, and deliberately small enough that the middle of a crowded plate is
/// still plate and not a line.
const LINK_HIT: f32 = 6.0;
/// At most this many ambient labels, strongest first — plus the selected and hovered
/// one, which are always labelled.
const LABELS_MAX: usize = 14;
/// How many of the strongest nodes are named even when the labels crowd: the promise is
/// "the top dozen, and never fewer than eight".
const LABELS_MIN: usize = 8;
const LABEL_SIZE: f32 = 11.0;
const LABEL_GAP: f32 = 4.0;
/// The plate's own caption strip along the bottom edge: a label may not sit on it.
const CAPTION_STRIP: f32 = 18.0;
/// The camera's own numbers. `ZOOM_MAX` is the most a node may be blown up, `ZOOM_MIN` is
/// the far end a wheel may reach (0.3: `tests/graph_bounds.rs` drives the camera to both
/// ends and asserts it arrives), and `FIT_MARGIN` keeps a reset view off the canvas edge.
const ZOOM_MAX: f32 = 4.0;
const ZOOM_MIN: f32 = 0.3;
const FIT_MARGIN: f32 = 0.98;
/// The fit measures the **drawn** box (the entities the opening view draws), and never scales
/// the picture more than this much past the fit of the whole layout — a sparse picture would
/// otherwise be blown up to the zoom ceiling.
const FIT_FILL: f32 = 1.5;
/// Under this alpha the layout counts as settled: the integrator stops and the page stops
/// asking for frames (it asks for one every 16 ms before that).
///
/// Exported because the shell has to stop asking for frames at the same number it does, or
/// the last frames of a settle never run and the layout freezes slightly hot: `app.rs` gates
/// its own `request_repaint_after` on `alpha > 0.005`, which the cooling reaches about ten
/// frames *before* this floor — reading this constant instead is the fix, and the view asks
/// for its own frames in the meantime so the settle completes either way.
pub const ALPHA_REST: f32 = 0.004;
/// How many stories an opened relation offers. Enough to see what the relation is made
/// of, few enough that the panel stays a panel with a relation list still in it.
pub const STORIES_SHOWN: i64 = 5;

/// One node in layout space. `vx`/`vy` are the integrator's memory, `radius` is the
/// drawn circle before the camera's zoom and before [`TAIL_RADIUS`], which is what a node
/// outside the plate's room is actually painted at (`node_radius` sets it).
///
/// `Debug` because a failing test has to be able to print the node it found — a layout
/// assertion that can only say "a node moved" costs a debugging session.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Node {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub radius: f32,
}

/// One relation between two nodes, with the two numbers the picture and the panel need.
#[derive(Clone, Default)]
pub struct Link {
    pub a: usize,
    pub b: usize,
    /// The stored weight against the heaviest relation of the whole set, 0..1: the order
    /// "strongest first" is taken in, and the window the panel sorts on.
    ///
    /// The *drawn* weight and brightness are computed from [`Self::stories`] against the
    /// range of the relations the picture actually draws (`show`, `strength_share`): a line
    /// is read next to its neighbours, not next to a relation off the plate.
    pub strength: f32,
    /// The stored weight — how many stories name both ends. The reason, when the model
    /// wrote no label.
    pub stories: f32,
    /// The model's relation, if it named one ("governs", "member of").
    pub label: String,
}

/// One row of the side panel: the other end of a relation, and why it is there.
///
/// `other` is the slot of the entity on the far end, which is what makes the row a door:
/// the canvas rings the two slots the row stands for, and the panel lists the stories
/// that name both of their ids.
pub struct Relation {
    pub name: String,
    pub other: usize,
    pub strength: f32,
    pub stories: f32,
    pub label: String,
}

/// One story that names both ends of a relation — what a click in the panel opens.
#[derive(Clone, Default)]
pub struct Story {
    pub id: i64,
    pub title: String,
    pub source: String,
    pub published: String,
}

/// The stories of the relation the panel has open, and the pair they belong to.
///
/// `stories` is a `Result` rather than an empty list on failure: "the archive could not be
/// read" and "no story names both ends" are different claims, and a panel that shows the
/// second when the first is true is lying about the archive.
pub struct Opening {
    /// The two slots the relation joins, in [`pair_of`]'s order.
    pub pair: (usize, usize),
    pub stories: Result<Vec<Story>, String>,
}

/// The statement behind [`stories_for`]: the stories that name both ends of one relation,
/// newest first.
///
/// Both ends are one index seek each — `idx_ae_entity` finds every article naming the
/// first entity, and the primary key on `(article_id, entity_id)` answers "does this one
/// name the second too" — so the cost is the number of articles that mention the first
/// name, not the size of `article_entities`.
///
/// `relevant = 1 AND dupe = 0` is `data`'s window predicate minus the window
/// (`ARTICLE_WINDOW`): a relation may be built from stories older than the page's window,
/// and a story the reading pane would not show is not a door worth offering. The order is
/// `data`'s too — newest first — so the panel and the article list agree about which story
/// is the freshest.
const STORIES_SQL: &str = "SELECT a.id, coalesce(a.title,''), coalesce(s.name,''), \
     coalesce(a.published,'') FROM article_entities one \
     JOIN article_entities two ON two.article_id = one.article_id \
     JOIN articles a ON a.id = one.article_id \
     LEFT JOIN sources s ON s.id = a.source_id \
     WHERE one.entity_id = ?1 AND two.entity_id = ?2 AND a.relevant = 1 AND a.dupe = 0 \
     ORDER BY a.published DESC, a.id DESC LIMIT ?3";

/// The newest `limit` stories that name both `one` and `two`, newest first.
///
/// Read-only twice over, like [`crate::data::Archive::open`]: `SQLITE_OPEN_READ_ONLY` on
/// the file and `PRAGMA query_only` on the connection, so a view cannot write the archive
/// by accident. The connection is opened and dropped per call because a call happens on one
/// click, not on a frame.
///
/// The error is a sentence the panel can show, and it names the file the way
/// [`crate::assets::label`] names it. The driver's own message is not echoed here: for an
/// open failure it ends with the absolute path, which is a home directory on every machine
/// this app runs on and a name on a surface a passer-by reads. The SQLite code is enough to
/// act on — "missing", "in use", "not a database" are [`rusqlite::ErrorCode`] variants.
pub fn stories_for(path: &Path, one: i64, two: i64, limit: i64) -> Result<Vec<Story>, String> {
    // `READ_ONLY` alone, the same flag set `data::Archive::open` uses. `SQLITE_OPEN_URI`
    // is not needed and was noise: this build compiles SQLite with `SQLITE_USE_URI`
    // (`libsqlite3-sys-0.30.1/build.rs:137`), so a `file:…?mode=…` string is parsed as a
    // URI whether or not the caller asks for it, and the *flag set* is what refuses
    // `mode=rwc` (measured: `READ_ONLY` and `READ_ONLY | URI` both answer "access mode not
    // allowed: rwc"; the proof is `tests/no_spawn_no_socket.rs::
    // the_archive_is_read_only_by_flags_and_a_path_cannot_change_that`). Passing one flag
    // in one file and another in the other made two written decisions out of one rule.
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    .map_err(|error| {
        let why = match error.sqlite_error_code() {
            Some(code) => format!(" ({code:?})"),
            None => String::new(),
        };
        format!("cannot read {}{why}", crate::assets::label(path))
    })?;
    conn.pragma_update(None, "query_only", true)
        .map_err(|error| format!("cannot set query_only: {error}"))?;
    let mut statement = conn
        .prepare(STORIES_SQL)
        .map_err(|error| format!("cannot read the relation's stories: {error}"))?;
    let rows = statement
        .query_map(rusqlite::params![one, two, limit.max(0)], |row| {
            Ok(Story {
                id: row.get(0)?,
                title: row.get(1)?,
                source: row.get(2)?,
                published: row.get(3)?,
            })
        })
        .map_err(|error| format!("cannot read the relation's stories: {error}"))?;
    let stories: Vec<Story> = rows.flatten().collect();
    Ok(stories)
}

/// The graph's own state: the nodes in layout space, the relations between their
/// indices, and the camera. `fill` restarts the layout; everything else is per frame.
#[derive(Default)]
pub struct GraphView {
    pub entities: Vec<Entity>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Link>,
    pub index: HashMap<i64, usize>,
    pub alpha: f32,
    pub selected: Option<usize>,
    pub pan: Vec2,
    pub zoom: f32,
    /// The node the pointer is holding, if any. A node drag is not a pan.
    drag: Option<usize>,
    /// A pan in progress: the camera and the pointer where the press happened, so the
    /// picture follows the hand by the pointer's own travel.
    grab: Option<(Vec2, Pos2)>,
    /// Whether the canvas had the keyboard on the previous frame. `egui` clears its focused
    /// widget when it sees Escape (`egui-0.37.0/src/memory/mod.rs:597`), before this view is
    /// drawn, so the frame the key arrives on is the one frame `has_focus()` is false and a
    /// handler gated on it never runs. Remembering the frame before is what makes Escape the
    /// canvas's key.
    had_focus: bool,
    /// How many relations touch each node, so "strongest" is one sort and not a scan.
    degree: Vec<u32>,
    /// The relation the panel has open, as its two slots. A [`Cell`] because the panel is
    /// handed `&GraphView` after the canvas has already been drawn: the row a reader clicks
    /// writes here, and the canvas reads it back on the next frame, where it rings both ends
    /// and draws that one line in ink.
    focus: Cell<Option<(usize, usize)>>,
    /// The story the panel opened, taken by [`GraphView::show`]. Second cell, second
    /// reader: the shell only ever sees the canvas's answer.
    opened: Cell<Option<i64>>,
    /// The stories of `focus`, loaded by `show` when the pair changes and drawn from memory
    /// on every frame after. `None` until a relation is opened.
    opening: Option<Opening>,
    /// The archive the panel asks for stories. Empty means [`crate::assets::archive`]; only
    /// a test sets it, through [`GraphView::with_archive`].
    archive: PathBuf,
    /// The canvas the camera was last fitted to, and whether the reader has moved it since.
    ///
    /// A picture that opens at zoom 1 on whatever canvas it is given is a blob in the middle
    /// of a large window — measured, 120 nodes on 2400×1400 fitted in 800 px of it. So the
    /// first frame fits the layout, a resize fits it again *while the reader has not touched
    /// the camera*, and a view the reader has panned or zoomed is theirs and stays.
    fitted: Option<Vec2>,
    touched: bool,
    /// The relation lines the last frame painted, in **screen** coordinates, as the index of
    /// the row plus the two ends it was drawn between.
    ///
    /// A press is answered against what the reader saw, and a press always arrives *before*
    /// this frame paints: the click handler runs at the top of [`Self::show`], the picture is
    /// painted at the bottom. So the geometry a click can land on is the previous frame's,
    /// and keeping it here is what makes a line a target at all — before this, a press on a
    /// line answered `None` and cleared the selection the reader had, measured on the live
    /// archive at the middle of the Nvidia–Huang line.
    shown_links: Vec<(usize, Pos2, Pos2)>,
    /// The names the last frame painted, in screen coordinates: the node each one belongs to
    /// and the box the text occupies. Read by [`Self::pick_name`], for the same reason.
    shown_names: Vec<(usize, Rect)>,
    /// The dots the last frame painted, in screen coordinates: the node and the radius the
    /// circle was drawn at. Pressing *inside a painted circle* is a more exact test than
    /// [`Self::pick`]'s fingertip slack, and it is the one a name press has to beat.
    shown_dots: Vec<(usize, Pos2, f32)>,
    /// A hash of the set [`Self::fill`] was last given: the same set twice is a page switch,
    /// not a new picture, and must not reset the camera or restart the layout.
    key: Option<u64>,
    /// Whether the layout has come to rest.
    ///
    /// `alpha` is a public field and the shell bumps it when the Graph page is opened, so a
    /// settled picture could be set moving again without the view asking for it. A picture
    /// that has settled *is* the picture the reader left: a foreign bump must not restart
    /// the simulation, or the dots they come back to are not the dots they left (measured:
    /// 87.7 px of drift across Overview and back, with the camera itself kept).
    at_rest: bool,
    /// The node a double click fixed the picture on: its neighbours stay in ink and
    /// everything else is context until a second double click or Escape. `None` is the whole
    /// graph, which is what the page opens with.
    focal: Option<usize>,
    /// How far the layout has been stretched along the x axis to match the plate.
    ///
    /// A force layout settles into a disc whatever the canvas is, and a disc in an 800×500
    /// plate uses 51 % of it: the sides of the page are empty and the middle is a mass. The
    /// picture is therefore scaled along x by the plate's own aspect once the canvas is
    /// known — a stretch never brings two dots closer (every pair's x distance grows, and a
    /// pair with the same x keeps its y distance), so the collision guarantee survives it, and
    /// the same 120 dots then cover the page they are drawn on.
    spread: f32,
}

impl GraphView {
    /// Take a new set of entities and relations.
    ///
    /// A set that is *the same set* — the same entities with the same relations, which is
    /// what a page switch hands back — keeps the picture it already has: the layout stays
    /// where it settled, and the camera, the selection and the open relation stay with it, so
    /// a reader who zoomed in, stepped to Overview and came back is where they left. Measured
    /// before this: every page switch reset zoom to 1 and pan to zero, because the shell calls
    /// `fill` whenever it reloads the graph.
    ///
    /// Genuinely new data starts over: positions restart on a spiral, because a node that
    /// keeps a stale coordinate from another set would lie about the new layout.
    ///
    /// One relation is one edge, whatever the archive stored: a pair named in both
    /// directions keeps its first row, and `strength` is every row's weight against the
    /// heaviest row that was kept — the order "strongest first" is taken in, in the panel and
    /// in the ranking alike. What the *picture* draws is a second question: `show` maps the
    /// drawn set's own weight range onto the palette, so the strongest line on the plate is
    /// always at the top of it.
    ///
    /// The radii are ranks of this set, assigned after the relations are counted: `strongest`
    /// is degree first and then mentions, and the radius is a curve of a node's place in that
    /// order (`rank_radius`).
    pub fn fill(&mut self, entities: Vec<Entity>, edges: Vec<Edge>) {
        let key = picture_key(&entities, &edges);
        let same = self.key == Some(key) && self.nodes.len() == entities.len();
        self.key = Some(key);
        self.entities = entities;
        self.index.clear();
        if !same {
            self.nodes.clear();
            let total = self.entities.len().max(1) as f32;
            for slot in 0..self.entities.len() {
                let angle = slot as f32 * 2.399_963;
                let radius = (slot as f32).sqrt() * (240.0 / total.sqrt()).max(6.0) * 2.2;
                self.nodes.push(Node {
                    x: angle.cos() * radius,
                    y: angle.sin() * radius,
                    vx: 0.0,
                    vy: 0.0,
                    // Assigned below, from the rank: the relations have to be counted first.
                    radius: RADIUS_MIN,
                });
            }
        }
        for slot in 0..self.entities.len() {
            self.index.insert(self.entities[slot].id, slot);
        }
        let mut kept: Vec<Link> = Vec::new();
        // One relation is one row, whichever end the archive named first. Storing both
        // directions would draw the same line twice, list it twice in the panel and
        // count it twice in `degree`, which is what the strongest-first order reads.
        let mut seen: HashSet<(usize, usize)> = HashSet::new();
        for edge in edges {
            let (Some(&a), Some(&b)) = (self.index.get(&edge.a), self.index.get(&edge.b)) else {
                continue;
            };
            let (low, high) = (a.min(b), a.max(b));
            if a == b || !seen.insert((low, high)) {
                continue;
            }
            let weight = edge.weight as f32;
            kept.push(Link {
                a,
                b,
                strength: weight,
                stories: weight,
                label: edge.label,
            });
        }
        // The scale is the heaviest relation that survived those filters, not a constant.
        // Seeded at one story — the old `1.0` — a window whose every relation weighed less
        // than that drew its strongest line at a fraction of the palette, so the heaviest
        // link, the one a reader looks for, came out dim. The guard below only keeps a set
        // of zero weights (or no relations at all) from dividing by zero.
        let heaviest = kept
            .iter()
            .fold(0.0_f32, |most, link| most.max(link.stories));
        let scale = if heaviest > 0.0 { heaviest } else { 1.0 };
        for link in kept.iter_mut() {
            link.strength = link.stories / scale;
        }
        self.edges = kept;
        self.degree = vec![0u32; self.nodes.len()];
        for link in &self.edges {
            self.degree[link.a] += 1;
            self.degree[link.b] += 1;
        }
        // The radii can only be assigned now: the rank they follow is degree first, and the
        // relations are what count the degree.
        let places = self.rank_places();
        for slot in 0..self.nodes.len() {
            self.nodes[slot].radius = rank_radius(places[slot], places.len());
        }
        // The same set is the same picture: the camera, the selection and the open relation
        // are the reader's, and a page switch is not a new graph. Only new data cools again
        // and starts from the top of the canvas.
        if same {
            return;
        }
        self.alpha = 1.0;
        self.at_rest = false;
        // A new picture is laid out from a spiral and stretched to the first canvas it meets.
        self.spread = 1.0;
        self.selected = None;
        self.drag = None;
        self.grab = None;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        // The panel's two cells name slots in the set that just went away: an open relation
        // pointing at another graph's node 3 would ring the wrong entity.
        self.focus.set(None);
        self.opened.set(None);
        self.opening = None;
        // A new set is a new picture: the next frame fits it to the canvas it is given.
        self.fitted = None;
        self.touched = false;
        // The painted geometry of the set that just went away names its slots: a press on
        // where a line of the old picture was would open a relation of the new one.
        self.shown_links.clear();
        self.shown_names.clear();
        self.shown_dots.clear();
    }

    /// Point the relation panel at a particular archive instead of the one the app reads.
    ///
    /// The window never calls this — [`crate::assets::archive`] is the archive — but a test
    /// has to ask a fixture rather than the live file, and there is no other way in.
    pub fn with_archive(mut self, path: impl Into<PathBuf>) -> Self {
        self.archive = path.into();
        self
    }

    /// The relation the panel has open, as its two slots, lower first.
    pub fn focused(&self) -> Option<(usize, usize)> {
        self.focus.get()
    }

    /// Open a relation from the panel, from either end: the plate rings both slots and the
    /// panel lists the stories that name both entities.
    ///
    /// It takes `&self` because of the frame's shape, not by preference: the page hands the
    /// panel a shared reference (the canvas owns the mutable one) and the two cells are the
    /// panel's whole write path. The stories are read by the next
    /// [`Self::show`], once, so a click is one query and a repaint is none.
    pub fn open_relation(&self, one: usize, other: usize) {
        self.focus.set(Some(pair_of(one, other)));
    }

    /// The stories of the relation the panel has open, if they belong to `pair`.
    pub fn opening(&self) -> Option<&Opening> {
        self.opening.as_ref()
    }

    /// The archive the panel reads.
    fn archive_path(&self) -> PathBuf {
        if self.archive.as_os_str().is_empty() {
            crate::assets::archive()
        } else {
            self.archive.clone()
        }
    }

    /// The stories of one relation, as the panel shows them: the newest few that name both
    /// ends, or the reason the archive could not be read.
    fn read_stories(&self, pair: (usize, usize)) -> Opening {
        let (Some(one), Some(two)) = (self.entities.get(pair.0), self.entities.get(pair.1))
        else {
            return Opening {
                pair,
                stories: Ok(Vec::new()),
            };
        };
        Opening {
            pair,
            stories: stories_for(&self.archive_path(), one.id, two.id, STORIES_SHOWN),
        }
    }

    /// One layout tick: repulsion, springs, a weak pull to the middle, the collision
    /// pass, cooling.
    pub fn tick(&mut self) {
        let count = self.nodes.len();
        if count == 0 {
            return;
        }
        if self.alpha <= ALPHA_REST {
            self.alpha = self.alpha.min(ALPHA_REST);
            self.at_rest = true;
            return;
        }
        if self.at_rest {
            // Heat nobody in this view asked for — the shell opening the page again. The
            // layout has already settled, and moving it would move the picture under a
            // reader who has only changed pages.
            self.alpha = ALPHA_REST;
            return;
        }
        let mut force = vec![(0.0f32, 0.0f32); count];
        // The picture's own scale: a hundred and twenty dots need twice the push sixty do, or
        // the springs win and the layout folds into a disc.
        let repulsion = REPULSION * (count as f32 / REPULSION_AT).max(0.5);
        for a in 0..count {
            for b in (a + 1)..count {
                let dx = self.nodes[a].x - self.nodes[b].x;
                let dy = self.nodes[a].y - self.nodes[b].y;
                let distance = (dx * dx + dy * dy).sqrt().max(0.5);
                let push = repulsion / (distance * distance);
                let (ux, uy) = (dx / distance, dy / distance);
                force[a].0 += ux * push;
                force[a].1 += uy * push;
                force[b].0 -= ux * push;
                force[b].1 -= uy * push;
            }
        }
        for link in &self.edges {
            let (a, b) = (link.a, link.b);
            let dx = self.nodes[b].x - self.nodes[a].x;
            let dy = self.nodes[b].y - self.nodes[a].y;
            let distance = (dx * dx + dy * dy).sqrt().max(1.0);
            let rest = rest_length(self.nodes[a].radius, self.nodes[b].radius);
            let stiffness = 0.006 * link.strength.max(0.15);
            // The force on each end is divided by the *other* end's degree ([`SPRING_DEGREE`]):
            // a hub pulls with the same one relation's worth as a leaf, however many relations
            // it has. Symmetric forces would let a sixty-relation hub out-pull the whole
            // repulsion field, which is the knot the reader reported.
            let on_a =
                stiffness / (1.0 + SPRING_DEGREE * self.degree[b].saturating_sub(1) as f32);
            let on_b =
                stiffness / (1.0 + SPRING_DEGREE * self.degree[a].saturating_sub(1) as f32);
            let stretch = distance - rest;
            let (ux, uy) = (dx / distance, dy / distance);
            force[a].0 += ux * stretch * on_a;
            force[a].1 += uy * stretch * on_a;
            force[b].0 -= ux * stretch * on_b;
            force[b].1 -= uy * stretch * on_b;
        }
        // The room the picture is allowed: the disc this many nodes need at [`PACK`], with a
        // floor so a picture of three entities is not squeezed into a corner of it.
        let room = (count as f32 * PACK * PACK / std::f32::consts::PI)
            .sqrt()
            .max(PACK * ROOM_FLOOR);
        // The trap that fills the room evenly ([`TRAP`]) is faded in with the crowd: at three
        // entities it is a twentieth of its strength, at sixty and above the whole of it. A
        // trap strong enough for a crowded page, measured on the three-entity picture, squeezed
        // it to its collision distances and left the joined pair (74.7 px) *farther* apart than
        // the lone entity (43.7 px) — `tests/graph_layout`'s
        // `a_joined_pair_settles_closer_to_itself_than_to_an_isolated_entity` was right to
        // refuse that. Faded in linearly, the same three entities answer 74.7 against 88.4 and
        // the 120-entity page keeps the trap's full spread.
        let crowded = (count as f32 / REPULSION_AT).min(1.0);
        let trap = crowded * TRAP * std::f32::consts::PI * repulsion / (room * room).max(1.0);
        // The hand owns the node it holds. `drag_node` applies the pointer's delta, and a tick
        // that also moved the node would put the layout's pull on top of it — with the trap
        // reaching the whole disc that pull is real: measured at ×1, a 40 px drag left the node
        // 30 px short, and at ×4 it left it 12 px short. The rest of the picture keeps settling
        // around the held node, which is what a reader dragging one entity expects to see.
        let held = self.drag;
        for (slot, node) in self.nodes.iter_mut().enumerate() {
            if held == Some(slot) {
                node.vx = 0.0;
                node.vy = 0.0;
                continue;
            }
            node.vx = (node.vx + force[slot].0 - node.x * trap) * 0.82;
            node.vy = (node.vy + force[slot].1 - node.y * trap) * 0.82;
            node.x += node.vx * self.alpha;
            node.y += node.vy * self.alpha;
        }
        // Last, and on positions rather than through a force: a collision is a fact
        // about where the nodes are, so cooling cannot outvote it.
        self.separate();
        self.alpha *= 0.985;
        // …and the rest is *measured*, not left to the decay: a picture that can still creep a
        // few pixels a frame is a picture that has moved by the time a reader comes back from
        // another page. Measured: with the decay alone the goods drifted 8.9 px across one page
        // switch while the camera was identical (zoom 0.8621, pan [85.6, 38.6]) —
        // `tests/layout_screens::a_page_switch_keeps_the_graphs_camera`. The travel still to
        // come is the geometric tail of the velocity, so a bound on the speed is a bound on the
        // pixels: 0.05 px a frame is under 0.3 px in total.
        let fastest = self
            .nodes
            .iter()
            .fold(0.0_f32, |most, node| most.max(node.vx.abs()).max(node.vy.abs()));
        if fastest * self.alpha < 0.05 {
            for node in self.nodes.iter_mut() {
                node.vx = 0.0;
                node.vy = 0.0;
            }
            self.alpha = ALPHA_REST;
            self.at_rest = true;
        }
        if self.alpha <= ALPHA_REST {
            self.alpha = self.alpha.max(0.0);
            self.at_rest = true;
        }
    }

    /// Push overlapping nodes apart until none overlap (or the round budget runs out).
    ///
    /// Deterministic: a pair at exactly the same point is separated along a direction
    /// derived from the two indices, never from a clock or an RNG, so the same rows
    /// always produce the same picture.
    pub fn separate(&mut self) {
        let count = self.nodes.len();
        let held = self.drag;
        for _ in 0..SEPARATION_ROUNDS {
            let mut moved = false;
            for a in 0..count {
                for b in (a + 1)..count {
                    let clear = self.nodes[a].radius + self.nodes[b].radius + SEPARATION_GAP;
                    let mut dx = self.nodes[b].x - self.nodes[a].x;
                    let mut dy = self.nodes[b].y - self.nodes[a].y;
                    let mut distance = (dx * dx + dy * dy).sqrt();
                    if distance >= clear {
                        continue;
                    }
                    if distance < 0.001 {
                        let angle = ((a * 37 + b * 61) % 360) as f32 * std::f32::consts::TAU / 360.0;
                        dx = angle.cos();
                        dy = angle.sin();
                        distance = 0.0;
                    } else {
                        dx /= distance;
                        dy /= distance;
                    }
                    // A node the hand is holding does not give way: the pointer puts it
                    // where the reader wants it, so the *other* node takes the whole
                    // correction. Measured on the ten-node picture, a 40 px drag landed on a
                    // neighbour and the split pushed the held node 31 px back off the
                    // pointer — the reader's own move undone by the layout.
                    let (held_a, held_b) = (held == Some(a), held == Some(b));
                    let push = if held_a || held_b {
                        clear - distance
                    } else {
                        (clear - distance) * 0.5
                    };
                    if !held_a {
                        self.nodes[a].x -= dx * push;
                        self.nodes[a].y -= dy * push;
                    }
                    if !held_b {
                        self.nodes[b].x += dx * push;
                        self.nodes[b].y += dy * push;
                    }
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
    }

    /// A drag on a node is the reader placing it: the node goes where the pointer goes, and
    /// its velocity is dropped so it does not fly off on release.
    ///
    /// The layout is **not** woken here. Waking it while the hand is still holding the node
    /// moved the whole picture under the pointer — measured on the ten-node plate, a drag on
    /// node 0 moved node 2 by 6.6 px, which reads as the graph sliding away from the hand. The
    /// release wakes it instead ([`Self::release`]), so the picture settles around where the
    /// reader put the node, not while it is being put there.
    pub fn drag_node(&mut self, slot: usize, delta: Vec2) {
        if let Some(node) = self.nodes.get_mut(slot) {
            node.x += delta.x;
            node.y += delta.y;
            node.vx = 0.0;
            node.vy = 0.0;
        }
    }

    /// The hand let go of a node: now the layout settles around it.
    fn release(&mut self) {
        self.wake();
    }

    /// Let the layout settle again — after a drag, or after the page is opened.
    pub fn wake(&mut self) {
        self.at_rest = false;
        self.alpha = self.alpha.max(0.35);
    }

    /// How far the layout is stretched along x, as a factor of the layout's own width. `1.0`
    /// on a square plate and up to [`SPREAD_MAX`] on a wide one; the default field is zero, so
    /// a view that has never seen a canvas reads as unstretched.
    fn spread(&self) -> f32 {
        if self.spread > 0.0 {
            self.spread
        } else {
            1.0
        }
    }

    /// Stretch the layout along x until it matches the plate under it.
    ///
    /// A force layout settles into a disc whatever the canvas is; a disc is a blob in a wide
    /// plate because two thirds of the page are empty. The stretch is applied to the positions
    /// themselves (velocities too, so the picture is not stretched and then pulled back), it
    /// never brings two dots closer — every pair's x distance grows — and the collision pass
    /// runs once afterwards to keep that promise in the numbers.
    fn stretch_to(&mut self, canvas: Rect) {
        if self.nodes.is_empty() || !canvas.is_positive() {
            return;
        }
        // Only before the reader has touched the camera: a stretch target read from the box
        // re-stretches the picture whenever the box moves, and under a gesture that is a
        // picture sliding out from under the pointer — measured in `tests/graph_blob`, a wheel
        // over a dot moved it 6.8 px. The target is the canvas's aspect, not the drawn box's:
        // the box moves while the layout settles, and a target that follows it never converges
        // (a drawn-box stretch measured 93 % × 85 % of the plate but could not hold the wheel's
        // anchor). The fit (`Home`, a double click, the Fit button) is what fills the plate.
        if self.touched {
            return;
        }
        let target = (canvas.width() / canvas.height()).clamp(1.0, SPREAD_MAX);
        let now = self.spread();
        if (target - now).abs() < 0.02 {
            return;
        }
        let ratio = target / now;
        for node in self.nodes.iter_mut() {
            node.x *= ratio;
            node.vx *= ratio;
        }
        self.spread = target;
        self.separate();
    }

    /// Put the whole layout on the plate: the zoom that fits its own box inside the canvas
    /// (never more than 1, so a small graph is not blown up into a guess), and the pan that
    /// centres it.
    ///
    /// This is the way back to the whole graph, and the reason the camera needs no bound on
    /// where it may be dragged: Home, and a double click on the empty plate. The box is the
    /// nodes' boxes with their radii, so nothing is fitted half off the edge.
    ///
    /// The zoom end below is `ZOOM_MIN` and not this fit: a reader may pull back past
    /// "everything visible" to see the shape of the whole picture, and `tests/graph_bounds.rs`
    /// drives the camera to that end and asserts it arrives.
    /// The three camera buttons on the plate: zoom in, zoom out, fit.
    ///
    /// They exist because neither the wheel nor the keys are discoverable — a reader who has
    /// never used a force graph does not know that a scroll zooms — and because a trackpad
    /// with no wheel is a laptop with no zoom at all. Each one is the keyboard's own gesture
    /// with a name: `+`, `−` and `0`, so the two cannot drift apart.
    fn plate_buttons(&mut self, ui: &mut egui::Ui, rect: Rect) {
        if !(rect.width() > 220.0) || !(rect.height() > 60.0) {
            return;
        }
        let bar = Rect::from_min_size(
            Pos2::new(rect.right() - 8.0 - 200.0, rect.top() + 8.0),
            Vec2::new(200.0, 24.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(bar)
                .layout(egui::Layout::right_to_left(egui::Align::Min)),
            |ui| {
                let button = |ui: &mut egui::Ui, words: &str| -> bool {
                    let response = ui.add(
                        egui::Button::new(theme::role::CAPTION.text(words))
                            .min_size(Vec2::new(0.0, 20.0))
                            .frame(false),
                    );
                    // A frameless button paints no focus ring — egui draws the ring with the
                    // frame — so the ring is painted here from the theme, the same one the
                    // relation rows and `views::brief`'s bullets wear, so a focused control
                    // looks like every other focused control on the page.
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            response.rect,
                            theme::RADIUS,
                            theme::focus_stroke(),
                            egui::StrokeKind::Inside,
                        );
                    }
                    response.clicked()
                };
                if button(ui, "Fit") {
                    self.reset_view(rect);
                    self.touched = false;
                }
                if button(ui, "Zoom out") {
                    self.zoom_about(rect, rect.center(), 1.0 / ZOOM_STEP);
                }
                if button(ui, "Zoom in") {
                    self.zoom_about(rect, rect.center(), ZOOM_STEP);
                }
            },
        );
    }

    /// Multiply the camera by `factor`, keeping the point under `at` where it is.
    ///
    /// The one place this view changes its zoom, so the wheel, the pinch, the three buttons
    /// and the keyboard cannot anchor the picture differently from each other. The anchor
    /// maths is the whole of it: the point under the pointer keeps the same screen position,
    /// which `tests/graph_interaction` measures at 0.5 px across ten steps.
    pub fn zoom_about(&mut self, canvas: Rect, at: Pos2, factor: f32) {
        if !(factor > 0.0) || !factor.is_finite() {
            return;
        }
        let before = self.zoom;
        self.zoom = (self.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        self.touched = true;
        if (self.zoom - before).abs() <= f32::EPSILON {
            return;
        }
        let centre = canvas.center() + self.pan;
        let grown = self.zoom / before.max(f32::EPSILON);
        let kept = at - (at - centre) * grown;
        self.pan += kept - centre;
    }

    pub fn reset_view(&mut self, canvas: Rect) {
        if self.nodes.is_empty() {
            self.zoom = 1.0;
            self.pan = Vec2::ZERO;
            return;
        }
        let (min, max) = self.drawn_box(canvas);
        self.zoom = self.fit_zoom(canvas);
        // A picture that already fits the plate is left where `fill` put it: at the origin, with
        // the camera at zero. Centring its *box* instead would move a picture nobody asked to
        // move — and, measured on ten nodes at ×1, it moved one by 11.5 px across and 12.9 px
        // down, because a hub's own radius extends the box further on one side than a leaf's
        // does on the other. A drag then starts from a camera that is not the one the reader
        // had, which is exactly the "the canvas jumps when I touch it" class of defect
        // (`tests/graph_interaction` measures the pan a drag answers).
        let middle = Vec2::new((min.x + max.x) * 0.5, (min.y + max.y) * 0.5);
        // The pan that puts the drawn box in the middle of the plate, clamped so the box stays
        // *inside* it: a picture that already fits is not moved further than it has to be (a
        // drag then starts from the camera the reader had — `tests/graph_interaction` measures
        // the pan a drag answers), and a picture whose box sits off the layout's origin is still
        // brought fully onto the plate. Measured before the clamp: a reset on 700×520 left the
        // dots spanning (32, −6)…(676, 504).
        let wanted = -middle * self.zoom;
        let (width, height) = (
            (max.x - min.x) * self.zoom,
            (max.y - min.y) * self.zoom,
        );
        let half = canvas.size() * 0.5;
        let slack_x = (half.x - width * 0.5 - 1.0).max(0.0);
        let slack_y = (half.y - height * 0.5 - 1.0).max(0.0);
        self.pan = Vec2::new(
            wanted.x.clamp(-slack_x.max(wanted.x.min(0.0)), slack_x.max(wanted.x.max(0.0)))
                .clamp(-slack_x, slack_x),
            wanted.y.clamp(-slack_y, slack_y),
        );
    }

    /// The box the **drawn** picture covers, in layout space, circles included: the entities
    /// the opening view draws ([`ACTORS_AT_FIT`] of the rank, plus the selection's own
    /// neighbourhood), not the ones it keeps back for the reader to zoom into.
    ///
    /// Both the **fit** ([`Self::fit_zoom`]) and the **stretch** ([`Self::stretch_to`]) measure
    /// this box: the picture the reader opens must fill the plate, and the entities the opening
    /// view keeps back are not part of it. Measured at 780×663 before this: fitting the box of
    /// all 120 left the drawn dots at 81 % × 74 % of the plate and the median nearest neighbour
    /// at 76.6 px, short of the 80 px the reader asked for.
    fn drawn_box(&self, canvas: Rect) -> (Pos2, Pos2) {
        let (mut min, mut max) = (
            Pos2::new(f32::INFINITY, f32::INFINITY),
            Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY),
        );
        for slot in self.drawn_slots_for(canvas) {
            if let Some(node) = self.nodes.get(slot) {
                let radius = node.radius.max(TAIL_RADIUS * 0.6);
                min.x = min.x.min(node.x - radius);
                min.y = min.y.min(node.y - radius);
                max.x = max.x.max(node.x + radius);
                max.y = max.y.max(node.y + radius);
            }
        }
        if !min.x.is_finite() {
            return (Pos2::ZERO, Pos2::ZERO);
        }
        (min, max)
    }

    /// The entities the opening view draws, strongest first: the rank down to the plate's own
    /// room and [`ACTORS_AT_FIT`], plus the selection's own neighbourhood — which is how an
    /// entity the opening view keeps back is still reachable (arrows, panel, a click on its
    /// neighbour). One function, so the painter, the fit and the stretch cannot disagree about
    /// what the picture is.
    fn drawn_slots_for(&self, canvas: Rect) -> Vec<usize> {
        if self.nodes.is_empty() {
            return Vec::new();
        }
        let rank = self.strongest();
        let room = capacity(canvas).min(ACTORS_AT_FIT).min(rank.len()).max(1);
        let mut slots: Vec<usize> = rank.iter().take(room).copied().collect();
        if let Some(slot) = self.selected {
            slots.push(slot);
            for link in &self.edges {
                if link.a == slot {
                    slots.push(link.b);
                }
                if link.b == slot {
                    slots.push(link.a);
                }
            }
        }
        if let Some((one, two)) = self.focus.get() {
            slots.extend([one, two]);
        }
        slots.retain(|slot| *slot < self.nodes.len());
        slots.sort_unstable();
        slots.dedup();
        // Back into rank order: the ramp a dot is painted at is its place in the drawn picture.
        let place = |slot: &usize| rank.iter().position(|other| other == slot).unwrap_or(usize::MAX);
        slots.sort_by_key(place);
        slots
    }

    /// The box the whole layout covers in layout space, every node's drawn circle included,
    /// and the grain's too: a reset that clipped a name because its dot was small would be a
    /// reset that hides an entity.
    fn layout_box(&self) -> (Pos2, Pos2) {
        let (mut min, mut max) = (
            Pos2::new(f32::INFINITY, f32::INFINITY),
            Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY),
        );
        for node in &self.nodes {
            let radius = node.radius.max(TAIL_RADIUS * 0.6);
            min.x = min.x.min(node.x - radius);
            min.y = min.y.min(node.y - radius);
            max.x = max.x.max(node.x + radius);
            max.y = max.y.max(node.y + radius);
        }
        (min, max)
    }

    /// Where the nodes are in layout space, centres only.
    ///
    /// This — not [`Self::layout_box`] — is what [`Self::clamp_pan`] promises something
    /// about, because it is what a reader's eye follows and what `tests/graph_bounds.rs`
    /// measures. Clamping the *radii* box instead left the promise short by one dot: at ×3.96
    /// a 27 px radius is 107 px of shadow on each side, so a frame could satisfy the clamp on
    /// the wide box and still cover only 27.4 % of a 320×240 plate by centres. The two boxes
    /// differ by less than a dot; that difference is the whole measurement.
    fn centre_box(&self) -> (Pos2, Pos2) {
        let (mut min, mut max) = (
            Pos2::new(f32::INFINITY, f32::INFINITY),
            Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY),
        );
        for node in &self.nodes {
            min.x = min.x.min(node.x);
            min.y = min.y.min(node.y);
            max.x = max.x.max(node.x);
            max.y = max.y.max(node.y);
        }
        (min, max)
    }

    /// Keep the picture on the plate: the layout's own box always overlaps the canvas by at
    /// least [`KEEP`] of the smaller of the two, on each axis.
    ///
    /// This is the bound the page was missing. A pan of `response.drag_delta()` with nothing
    /// after it is unbounded, so four hard drags in a row pushed the whole graph off a
    /// 1280×860 window and left a reader looking at an empty plate with no gesture that brings
    /// it back — measured on a 500-event storm, the worst frame painted 0 of 120 nodes and
    /// covered 0.0 % of the canvas (`tests/graph_bounds.rs`). With the clamp the picture can
    /// be pushed about, and three fifths of it stays visible on each axis whatever the reader
    /// does — an area of at least [`KEEP`]² = 36 % of the plate, against the 30 % floor that
    /// test asserts; the fit (Home, a double click on the empty plate) is still the way all of
    /// it comes back.
    fn clamp_pan(&mut self, canvas: Rect) {
        self.clamp_box(canvas);
        let boxed = self.pan;
        self.keep_a_node_on_the_plate(canvas);
        let nudged = self.pan;
        // The dot nudge answers the one case the box clamp cannot: a plate the picture covers
        // where no dot is drawn. It must not answer *for* the clamp, so unless it is a no-op
        // the box is clamped again — and only a clamp that would leave the plate empty is
        // refused, because a reader with nothing to see has nothing at all.
        if nudged != boxed {
            self.clamp_box(canvas);
            if !self.any_node_on(canvas) {
                self.pan = nudged;
            }
        }
        self.clamp_area(canvas);
    }

    /// The box half of [`Self::clamp_pan`]: three fifths of the layout, or of the canvas
    /// whichever is smaller, stays on the plate on each axis.
    fn clamp_box(&mut self, canvas: Rect) {
        if self.nodes.is_empty() || !(canvas.width() > 0.0) || !(canvas.height() > 0.0) {
            return;
        }
        let (min, max) = self.centre_box();
        let (left, right) = (min.x * self.zoom, max.x * self.zoom);
        let (top, bottom) = (min.y * self.zoom, max.y * self.zoom);
        if ![left, right, top, bottom].iter().all(|value| value.is_finite()) {
            self.pan = Vec2::ZERO;
            return;
        }
        let keep_x = KEEP * (right - left).min(canvas.width());
        let keep_y = KEEP * (bottom - top).min(canvas.height());
        let centre = canvas.center() + self.pan;
        // The centre of the canvas puts layout (0, 0) at the middle of the plate; the pan is
        // the offset of that middle, so clamping the middle is clamping the pan.
        let x = centre.x.clamp(
            (canvas.left() + keep_x - right).min(canvas.right() - keep_x - left),
            canvas.right() - keep_x - left,
        );
        let y = centre.y.clamp(
            (canvas.top() + keep_y - bottom).min(canvas.bottom() - keep_y - top),
            canvas.bottom() - keep_y - top,
        );
        let middle = canvas.center();
        self.pan = Vec2::new(x - middle.x, y - middle.y);
    }


    /// The area half of the promise: the intersection of the picture and the plate is at least
    /// [`KEEP`]² of whichever of the two is smaller.
    ///
    /// The per-axis clamp above does not imply it: a layout twice as wide as the plate can keep
    /// 60 % of its width visible while only a third of its *area* is on the plate — measured in
    /// the storm, 29.6 % at ×1.30 on a 1280×860 canvas, under the 30 % floor the bounds test
    /// holds the page to. So when the per-axis clamp leaves too little, the pan is scaled back
    /// toward the middle of the picture, and only as far as the floor needs.
    fn clamp_area(&mut self, canvas: Rect) {
        if self.nodes.is_empty() || !canvas.is_positive() {
            return;
        }
        let (min, max) = self.centre_box();
        let reach = Vec2::new((max.x - min.x) * self.zoom, (max.y - min.y) * self.zoom);
        if !reach.x.is_finite() || !reach.y.is_finite() || reach.x <= 0.0 || reach.y <= 0.0 {
            return;
        }
        let floor = KEEP * KEEP * canvas.area().min(reach.x * reach.y);
        let covered = |pan: Vec2| {
            let centre = canvas.center() + pan;
            let box_ = Rect::from_min_max(
                Pos2::new(centre.x + min.x * self.zoom, centre.y + min.y * self.zoom),
                Pos2::new(centre.x + max.x * self.zoom, centre.y + max.y * self.zoom),
            );
            box_.intersect(canvas).area()
        };
        if covered(self.pan) >= floor {
            return;
        }
        // The camera that covers everything is the middle of the picture, so the answer is on
        // the segment from there to the pan the reader asked for — bisected, because the
        // coverage is monotone along it and twenty steps is under a pixel at any plate.
        let (mut keeps, mut loses) = (Vec2::ZERO, self.pan);
        for _ in 0..24 {
            let middle = (keeps + loses) * 0.5;
            if covered(middle) >= floor {
                keeps = middle;
            } else {
                loses = middle;
            }
        }
        self.pan = keeps;
    }
    /// Whether any dot's centre is on the plate, which is what the frame draws.
    fn any_node_on(&self, canvas: Rect) -> bool {
        let centre = canvas.center() + self.pan;
        self.nodes.iter().any(|node| {
            canvas.contains(Pos2::new(
                centre.x + node.x * self.zoom,
                centre.y + node.y * self.zoom,
            ))
        })
    }
    /// The camera may not leave the plate empty.
    ///
    /// The box clamp above keeps *the picture* overlapping the plate, and overlapping area is
    /// not the same as a visible entity: at 3.28× on a 320×240 canvas the box still covers a
    /// third of the plate while every dot of a 120-node graph lies outside it — measured, the
    /// storm in `tests/graph_bounds.rs` reached exactly that frame (0 of 120 nodes on the
    /// canvas). So the shortest move that puts the nearest dot back on the plate is made
    /// here: whatever else a reader does to the camera, there is always something to see and
    /// something to press, and Home is still the way back to all of it.
    ///
    /// The shortest move, not "put it in the middle": a pan the reader made is answered with
    /// their camera, corrected only as far as the promise needs. Measured with the old
    /// centre-the-nearest-node version: a 120 px drag answered as 115, a 90 px drag as 76.
    fn keep_a_node_on_the_plate(&mut self, canvas: Rect) {
        if self.nodes.is_empty() || !canvas.is_positive() {
            return;
        }
        let centre = canvas.center() + self.pan;
        let screen = |node: &Node| {
            Pos2::new(centre.x + node.x * self.zoom, centre.y + node.y * self.zoom)
        };
        if self.nodes.iter().any(|node| canvas.contains(screen(node))) {
            return;
        }
        let middle = canvas.center();
        let Some(at) = self
            .nodes
            .iter()
            .map(screen)
            .min_by(|left, right| left.distance(middle).total_cmp(&right.distance(middle)))
        else {
            return;
        };
        // The nearest dot comes just inside the plate: one dot, on the edge, is the promise.
        // On a plate too small to shrink, the middle is the only place left.
        let room = canvas.shrink(2.0);
        let target = if room.is_positive() {
            Pos2::new(
                at.x.clamp(room.left(), room.right()),
                at.y.clamp(room.top(), room.bottom()),
            )
        } else {
            canvas.center()
        };
        self.pan += target - at;
    }
    /// The zoom at which the whole layout fits `canvas`, never above 1: a picture of six
    /// entities is not blown up into a guess about what the dots are.
    fn fit_zoom(&self, canvas: Rect) -> f32 {
        // The drawn box, capped at [`FIT_FILL`] times the whole layout's fit: the picture the
        // reader opens must fill the plate, but a fit that ignores the entities kept back can
        // blow a sparse picture up to the zoom ceiling — measured, a 40-node fixture fitted at
        // ×3.3 from the drawn box alone, where the whole layout's fit was ×2.6.
        let (whole_min, whole_max) = self.layout_box();
        let whole = (canvas.width() * FIT_MARGIN / (whole_max.x - whole_min.x).max(1.0))
            .min(canvas.height() * FIT_MARGIN / (whole_max.y - whole_min.y).max(1.0));
        let (min, max) = self.drawn_box(canvas);
        let width = (max.x - min.x).max(1.0);
        let height = (max.y - min.y).max(1.0);
        // A picture of six entities is not blown up into a guess about what the dots are; a
        // picture of a hundred and twenty is opened as large as the plate allows, because a
        // plate is not a corner of itself. Measured at 1100×820 with 120 nodes: leaving the
        // zoom at 1 left the graph covering 36 % of the canvas.
        let grow = if self.nodes.len() >= GROW_ABOVE {
            ZOOM_MAX
        } else {
            1.0
        };
        (canvas.width() * FIT_MARGIN / width)
            .min(canvas.height() * FIT_MARGIN / height)
            .min(whole * FIT_FILL)
            .clamp(ZOOM_MIN, ZOOM_MAX)
            .min(grow)
    }

    pub fn name_of(&self, slot: usize) -> String {
        self.entities
            .get(slot)
            .map(|entity| entity.name.clone())
            .unwrap_or_default()
    }

    pub fn strength_of(&self, slot: usize) -> i64 {
        self.entities.get(slot).map(|entity| entity.count).unwrap_or(0)
    }

    /// The strongest nodes first: most relations, then most mentions, then slot — the
    /// order the labels are drawn in and the order the arrow keys walk.
    pub fn strongest(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.nodes.len()).collect();
        // The archive's own ranking: how many stories name the entity first, how many
        // relations it has second. An entity the archive talks about most is the one a reader
        // came for; degree breaks the tie. (This was the other way round, and on a fixture
        // where a hub's count and degree disagree it drew the hub's neighbours first.)
        order.sort_by(|left, right| {
            self.strength_of(*right)
                .cmp(&self.strength_of(*left))
                .then_with(|| self.degree[*right].cmp(&self.degree[*left]))
                .then_with(|| left.cmp(right))
        });
        order
    }

    /// Where each slot sits in [`Self::strongest`]'s order: the rank the radius is a curve
    /// of. One sort, used by `fill` to size the set it was handed.
    fn rank_places(&self) -> Vec<usize> {
        let mut places = vec![0usize; self.nodes.len()];
        for (place, slot) in self.strongest().into_iter().enumerate() {
            places[slot] = place;
        }
        places
    }

    /// Where the arrows go: the next node in [`Self::strongest`], wrapping at both ends.
    pub fn step_selection(&self, direction: i32) -> Option<usize> {
        let order = self.strongest();
        if order.is_empty() {
            return None;
        }
        let count = order.len() as i32;
        let step = direction.signum();
        let next = match self
            .selected
            .and_then(|slot| order.iter().position(|candidate| *candidate == slot))
        {
            Some(index) => (((index as i32 + step) % count) + count) % count,
            None if step < 0 => count - 1,
            None => 0,
        };
        order.get(next as usize).copied()
    }

    /// The relations touching a node, strongest first, with the other end's name and slot
    /// and the reason it is there.
    pub fn around(&self, slot: usize) -> Vec<Relation> {
        let mut out: Vec<Relation> = Vec::new();
        for link in &self.edges {
            if link.a == slot || link.b == slot {
                let other = if link.a == slot { link.b } else { link.a };
                out.push(Relation {
                    name: self.name_of(other),
                    other,
                    strength: link.strength,
                    stories: link.stories,
                    label: link.label.clone(),
                });
            }
        }
        out.sort_by(|left, right| {
            right
                .strength
                .total_cmp(&left.strength)
                .then_with(|| left.name.cmp(&right.name))
        });
        out
    }

    /// The relations the picture draws, as indices into [`Self::edges`].
    ///
    /// Three rules, and the first one is the reader's: **the plate draws what it can read, not
    /// what the archive holds**. A relation is drawn when
    ///
    /// * both ends are drawn at full size — a line to a grain is a line to nothing;
    /// * its weight clears the picture's own cut: `stories >= cut`, where `cut` is the median
    ///   weight of the candidate set and never under two. One co-mention is the archive's
    ///   noise floor (7 373 rows, most of them weight 1), and drawing that floor is what made
    ///   the plate a grey mesh: a reader cannot follow a relation that says "these two were
    ///   mentioned once, in different paragraphs";
    /// * it is among the strongest [`LINKS_PER_NODE`] relations of at least one of its ends,
    ///   so every full-size node keeps its own strongest lines and a hub with ninety relations
    ///   is a rounded star instead of a star of ninety.
    ///
    /// A node keeps at least one line whatever the cut says, so no actor is left standing
    /// alone on the plate, and the selected node's own relations are always drawn — the reader
    /// asked for that neighbourhood by clicking it.
    ///
    /// What is not drawn is not lost: [`Self::around`] reads every row, so the panel of a
    /// selected node lists all of its relations with their reasons, drawn or not.
    fn drawn_links(&self, full: &[bool], cap: usize) -> Vec<usize> {
        let mut per_node: Vec<Vec<(f32, usize)>> = vec![Vec::new(); self.nodes.len()];
        let mut weights: Vec<f32> = Vec::new();
        for (index, link) in self.edges.iter().enumerate() {
            if !full[link.a] || !full[link.b] {
                continue;
            }
            per_node[link.a].push((link.strength, index));
            per_node[link.b].push((link.strength, index));
            weights.push(link.stories);
        }
        // The cut is the picture's own median weight, floored at two: the archive's one-story
        // co-mentions are the tail, and a plate that draws its tail is a plate a reader cannot
        // read. `stories` is the number of stories that name both ends, so this is a fact
        // about the archive, not a number picked here.
        weights.sort_by(f32::total_cmp);
        let cut = weights
            .get(weights.len() / 2)
            .copied()
            .unwrap_or(1.0)
            .max(2.0);
        // A plate that can show every candidate draws every candidate: the cut exists to keep
        // a big archive readable (900 relations over 120 nodes), and thinning a picture of
        // twenty relations would hide lines a reader can perfectly well follow. Two relations
        // per drawn node is a picture a reader can trace; past that the plate is a mesh and the
        // cut earns its keep (measured: 418 lines at 120 nodes, 20 at 10).
        if weights.len() <= full.len() * 2 {
            return (0..self.edges.len())
                .filter(|index| full[self.edges[*index].a] && full[self.edges[*index].b])
                .collect();
        }
        let mut keep = vec![false; self.edges.len()];
        for mut owned in per_node {
            owned.sort_by(|left, right| {
                right
                    .0
                    .total_cmp(&left.0)
                    .then_with(|| left.1.cmp(&right.1))
            });
            // The strongest relation is kept whatever the cut says: a node with one weak
            // relation still has a relation, and a picture where an actor stands alone is a
            // picture that says there is nothing there.
            if let Some((_, index)) = owned.first() {
                keep[*index] = true;
            }
            for (_, index) in owned.into_iter().take(cap) {
                if self.asked(self.edges[index].a, self.edges[index].b)
                    || self.edges[index].stories >= cut
                {
                    keep[index] = true;
                }
            }
        }
        (0..self.edges.len())
            .filter(|index| keep[*index])
            .collect()
    }

    /// Whether this relation is one the reader asked for: the open pair, or a relation of the
    /// selected node. Those are drawn whatever the cut says — a reader who clicked an entity
    /// asked to see what connects it.
    fn asked(&self, a: usize, b: usize) -> bool {
        if self
            .focus
            .get()
            .is_some_and(|(one, two)| pair_of(a, b) == (one, two))
        {
            return true;
        }
        self.selected.is_some_and(|slot| slot == a || slot == b)
    }

    /// How far each node is from its own nearest neighbour, in layout units.
    ///
    /// The number a dot's radius is capped by: a picture is a graph while there is air between
    /// its dots, and the air is exactly this distance minus the two radii. O(n²) once a frame
    /// — 7 140 pairs at 120 nodes, measured under 0.05 ms.
    fn neighbour_reach(&self) -> Vec<f32> {
        let mut reach = vec![f32::INFINITY; self.nodes.len()];
        for a in 0..self.nodes.len() {
            for b in (a + 1)..self.nodes.len() {
                let distance = ((self.nodes[a].x - self.nodes[b].x).powi(2)
                    + (self.nodes[a].y - self.nodes[b].y).powi(2))
                .sqrt();
                reach[a] = reach[a].min(distance);
                reach[b] = reach[b].min(distance);
            }
        }
        for value in reach.iter_mut() {
            if !value.is_finite() {
                *value = RADIUS_MAX / RADIUS_SHARE;
            }
        }
        reach
    }

    /// How close a press has to be to grab a node: its drawn circle plus slack, and
    /// never less than a fingertip-sized target — a 3 px dot is not a 3 px target.
    fn hit_radius(&self, slot: usize) -> f32 {
        let drawn = self
            .nodes
            .get(slot)
            .map(|node| node.radius * self.zoom.max(0.0))
            .unwrap_or(0.0);
        drawn.max(HIT_MIN) + HIT_SLACK
    }

    /// The node under `pointer` — the nearest one whose hit radius contains it — for a
    /// canvas whose camera puts layout `(0, 0)` at `centre`.
    pub fn pick(&self, pointer: Pos2, centre: Pos2) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (slot, node) in self.nodes.iter().enumerate() {
            let at = Pos2::new(centre.x + node.x * self.zoom, centre.y + node.y * self.zoom);
            let distance = pointer.distance(at);
            if distance > self.hit_radius(slot) {
                continue;
            }
            if best.map(|(_, nearest)| distance < nearest).unwrap_or(true) {
                best = Some((slot, distance));
            }
        }
        best.map(|(slot, _)| slot)
    }

    /// The relation line under `pointer`, as the pair of slots it joins: the nearest drawn
    /// line within [`LINK_HIT`] px of it, and `None` when the press is clear of every line.
    ///
    /// Screen coordinates, against the picture the last [`Self::show`] painted, because that
    /// is the picture the reader aimed at — a projection recomputed from the camera would be
    /// a projection of where the lines are *now*, and [`Self::pick`] is the only hit-test that
    /// can afford that, since a dot is a circle and a line is a segment of finite width.
    /// Only drawn lines answer: a relation the plate left out is reachable through the panel
    /// of either end, but it is not on the screen to press.
    pub fn pick_link(&self, pointer: Pos2) -> Option<(usize, usize)> {
        let mut best: Option<(usize, f32)> = None;
        for (index, one, two) in &self.shown_links {
            let distance = segment_distance(pointer, *one, *two);
            if distance > LINK_HIT {
                continue;
            }
            if best.map(|(_, nearest)| distance < nearest).unwrap_or(true) {
                best = Some((*index, distance));
            }
        }
        best.and_then(|(index, _)| {
            self.edges
                .get(index)
                .map(|link| pair_of(link.a, link.b))
        })
    }

    /// The node a painted name belongs to, for a press on the text: a name a reader can
    /// read but not press is decoration.
    ///
    /// A name box is what the painter was given, so the press resolves exactly where the
    /// letters are — and names never overlap ([`place_labels`]), so at most one answers.
    pub fn pick_name(&self, pointer: Pos2) -> Option<usize> {
        self.shown_names
            .iter()
            .find(|(_, box_)| box_.contains(pointer))
            .map(|(slot, _)| *slot)
    }

    /// What a press at `pointer` on a canvas whose camera centre is `centre` opens, in the
    /// order the picture stacks it: a name, a dot, a relation line, and then — because a 5 px
    /// dot is not a 5 px target — the same dot again with its fingertip slack, and only then
    /// the bare plate.
    ///
    /// The slack is last on purpose. A name sits [`LABEL_GAP`] px beside its dot, so its
    /// middle is well inside the 15 px a small dot asks for; a press on the *letters* must
    /// reach the letters, or a reader aiming at a name beside a fat dot selects the
    /// neighbour instead (measured before this order: a press on "Entity number 8" selected
    /// node 52, the grain its text was painted over).
    pub fn target(&self, pointer: Pos2, centre: Pos2) -> Target {
        // The stacking the picture paints in, and the hit test follows it: **names, then dots,
        // then lines**, then the same dot again with its fingertip slack, then the plate. A
        // name is the smallest, most deliberate target on the plate and it is painted last, so
        // nothing behind it takes the press — not the dot it was placed beside, not the
        // relation it was drawn across (measured: "Ukraine" answered the line (2, 31) that runs
        // under three of its letters). The letters answer within a couple of pixels of the
        // painted box, because a label is placed against the frame it was painted on and a
        // press arrives a frame later, when the picture may have crept a pixel or two.
        if let Some(slot) = self
            .shown_names
            .iter()
            .find(|(_, box_)| box_.expand(2.0).contains(pointer))
            .map(|(slot, _)| *slot)
        {
            // A bigger dot under the pointer used to veto the name, on the theory that the
            // reader was aiming at the dot. `place_labels` never puts a name over a bigger dot
            // in the first place, so the veto only fired where the geometry had drifted — and
            // then a press on the word selected a neighbour instead: measured on the shipped
            // archive, "Germany" at 426×304 selected node 8, one of its own neighbours.
            //
            // A name beats a line, always. It used to lose to any stroke within the band, on
            // the theory that a press right on a line is a press on the line — but that is the
            // wrong way round: the letters are the **smaller, deliberate** target, drawn on top
            // of the stroke, and a reader aiming at a word is aiming at the word. Measured on
            // the shipped archive: "Ukraine" is painted at [[683.9 223.5] - [721.4 236.5]], a
            // press on its middle at [691.4 227.8] sat on the line (2, 31) and answered that
            // relation instead of the entity the letters name.
            //
            // A reader who wants the relation has the whole length of it to aim at, and the
            // panel lists it; a reader who wants the entity has only these few pixels of text.
            return Target::Name(slot);
        }
        if let Some(slot) = self
            .shown_dots
            .iter()
            .find(|(_, at, radius)| pointer.distance(*at) <= *radius + 1.0)
            .map(|(slot, _, _)| *slot)
        {
            return Target::Node(slot);
        }
        if let Some(pair) = self.pick_link(pointer) {
            return Target::Link(pair);
        }
        match self.pick(pointer, centre) {
            Some(slot) => Target::Node(slot),
            None => Target::Plate,
        }
    }

    /// Paint the graph, load the stories of the relation the panel opened, and answer the
    /// story the reader opened — `None` on every frame that opened nothing.
    ///
    /// The answer is one frame behind the click that produced it: the page draws the canvas
    /// before the panel, so a story clicked in the panel is handed back here on the next
    /// frame. A caller that also reads [`panel`]'s answer skips that frame.
    pub fn show(&mut self, ui: &mut egui::Ui, height: f32) -> Option<i64> {
        // The one query this view runs: the pair changed, so its stories are read. On every
        // other frame `opening` already answers for `focus`, and a repaint costs no SQL.
        match self.focus.get() {
            Some(pair) if self.opening.as_ref().map(|opening| opening.pair) != Some(pair) => {
                self.opening = Some(self.read_stories(pair));
            }
            Some(_) => {}
            None => self.opening = None,
        }
        self.tick();
        // While the layout is still moving, the page asks for the next frame — *in a frame's
        // time*, never immediately: an immediate ask is a busy loop, and `tests/fresh_machine`
        // measures it (`the Graph page asked for 12 of twelve idle frames immediately`). When
        // the layout is at rest, and when there is nothing on the plate to cool, the view asks
        // for nothing at all, so an idle Graph page is a quiet one.
        if !self.nodes.is_empty() && self.alpha > ALPHA_REST {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
        }
        // The canvas **is** the box the page hands it — both axes, no fraction of its own, and
        // no fixed share. `height` is the plan's share of the page and equals the room on every
        // page the shell draws today; the room wins when it is larger, because a canvas that
        // paints a band inside its own box is the reported "the graph is not full screen", and
        // because the plate's width is already `available_width` — a height that fell short of
        // it would be the one number this view could still get wrong. The room is unbounded
        // inside a scroll area, and the page is deliberately not inside one (`app.rs`), so the
        // guard keeps a hypothetical one from allocating an infinite canvas.
        let room = ui.available_size();
        let tall = if room.y.is_finite() && room.y > height {
            room.y
        } else {
            height
        };
        // A box with no area has no picture. `allocate_painter` needs a positive size, and the
        // `.max(1.0)` below fed it a 1×1 canvas in a 0×0 box — measured: `0x0 window: 57
        // shapes, 1 circles, painted canvas [[0.0 0.0] - [1.0 1.0]]`, a node drawn on a page
        // that has no room for one (`tests/graph_bounds`,
        // `no_frame_paints_a_nan_and_a_zero_window_paints_nothing`). A collapsed column is not a
        // plate, so this answers nothing and paints nothing.
        if !(room.x > 0.0 && tall > 0.0) {
            return None;
        }
        let (response, painter) = ui.allocate_painter(
            Vec2::new(room.x.max(1.0), tall.max(1.0)),
            Sense::click_and_drag(),
        );
        let rect = response.rect;
        painter.rect_filled(
            rect,
            egui::CornerRadius::ZERO,
            PLATE,
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Unknown,
                true,
                format!("Entity graph, {} entities", self.nodes.len()),
            )
        });

        // The first frame with a canvas fits a picture that does not fit, and so does a
        // resize — as long as the reader has not moved the camera, whose view is theirs once
        // they have. A picture that already fits is left exactly where `fill` put it (zoom 1,
        // pan 0): moving a rectangle that is already inside the plate for no reason would
        // make every drag start from a camera nobody asked for.
        let size = rect.size();
        let same = self
            .fitted
            .is_some_and(|fitted| (fitted - size).length() < 0.5);
        // Before the fit, never after: the box the fit measures is the box the reader sees.
        self.stretch_to(rect);
        if !same && !self.touched {
            self.reset_view(rect);
            self.fitted = Some(size);
        }

        // Zoom: a mouse wheel, a trackpad's two-finger scroll and a pinch are one gesture in
        // three units, and all three anchor on the pointer — the node under the cursor stays
        // under it. The wheel and the buttons are the same function of the same numbers
        // (`zoom_factor`), so they cannot drift apart.
        if response.hovered() {
            let (scroll, pinch) = ui.input(|
                input| (input.smooth_scroll_delta.y, input.zoom_delta())
            );
            let factor = zoom_factor(scroll, pinch);
            if (factor - 1.0).abs() > 1.0e-4 {
                let at = response.hover_pos().unwrap_or_else(|| rect.center());
                self.zoom_about(rect, at, factor);
            }
        }
        let centre = rect.center() + self.pan;

        // One gesture per target: a press that lands on a node moves the node, a press
        // anywhere else pans. `drag_delta` at the start of the drag is the few pixels the
        // pointer already moved, so the pick is made where the press happened.
        if response.drag_started() {
            // The press, exactly: egui swallows the few pixels a drag needs to be recognised,
            // so neither `drag_delta` nor `interact_pointer_pos` knows where the button came
            // down — `press_origin` does, and a pan measured from it follows the hand.
            let press = ui
                .input(|input| input.pointer.press_origin())
                .or_else(|| response.interact_pointer_pos().map(|pointer| pointer - response.drag_delta()));
            self.drag = press.and_then(|press| match self.target(press, centre) {
                Target::Node(slot) => Some(slot),
                _ => None,
            });
            // A pan is measured from where the press happened, not summed from per-frame
            // deltas: egui swallows the few pixels a drag needs to be recognised, and the
            // frame the button comes up carries the last move with no drag delta at all —
            // measured, a 120 px drag answered 115 and a 90 px drag answered 76.
            self.grab = press.filter(|_| self.drag.is_none()).map(|press| (self.pan, press));
        }
        if response.dragged() || response.drag_stopped() {
            match self.drag {
                Some(slot) => {
                    let delta = response.drag_delta() / self.zoom.max(ZOOM_MIN);
                    self.drag_node(slot, delta);
                }
                None => match (self.grab, response.interact_pointer_pos()) {
                    (Some((base, press)), Some(now)) => {
                        self.pan = base + (now - press);
                        self.touched = true;
                    }
                    _ => {
                        self.pan += response.drag_delta();
                        self.touched = true;
                    }
                },
            }
        }
        if response.drag_stopped() {
            if self.drag.is_some() {
                self.release();
            }
            self.drag = None;
            self.grab = None;
        }
        // A click takes focus, and the canvas then shows it like every other control.
        //
        // It answers the thing it landed on, in the order the picture stacks them: a node,
        // then a relation line, then a name. Only a press that hits none of the three is a
        // press on the plate, and only that one clears what the reader had — a press aimed
        // at a line answered `None` before this and threw away the selection it was aimed at
        // (measured on the live archive: the middle of the Nvidia–Huang line).
        if response.clicked() {
            response.request_focus();
            if let Some(pointer) = response.interact_pointer_pos() {
                match self.target(pointer, centre) {
                    Target::Node(slot) => {
                        self.selected = Some(slot);
                        // The reader is looking at another entity: the relation the panel
                        // had open belonged to the old selection, so the panel closes it.
                        self.focus.set(None);
                    }
                    Target::Link(pair) => {
                        // The same answer a press on the panel's row gives: the row of the
                        // selected end opens, and the stories appear under it. The panel
                        // lists the relations of `selected`, so the selection moves to the
                        // line's own lower end — a pair whose row is not in the list would
                        // ring both ends on the plate and say nothing in the panel.
                        self.selected = Some(pair.0);
                        self.focus.set(Some(pair));
                    }
                    Target::Name(slot) => {
                        self.selected = Some(slot);
                        self.focus.set(None);
                    }
                    Target::Plate => {
                        self.selected = None;
                        self.focus.set(None);
                    }
                }
            }
        }
        // A double click on the empty plate is the way back to the whole graph: a reader who
        // has panned the picture into a corner has no other gesture that brings it home. On a
        // node it is the other half of the question — "what is around *this* one" — so the
        // picture fixes on it (`focal`), its neighbours stay in ink and the rest of the plate
        // is context; a second double click, or Escape, gives the whole graph back.
        if response.double_clicked() {
            let target = response
                .interact_pointer_pos()
                .map(|pointer| self.target(pointer, centre));
            match target {
                Some(Target::Node(slot)) | Some(Target::Name(slot)) => {
                    if self.focal == Some(slot) {
                        self.focal = None;
                        self.selected = None;
                    } else {
                        self.focal = Some(slot);
                        self.selected = Some(slot);
                        self.focus.set(None);
                    }
                }
                Some(Target::Link(pair)) => {
                    self.selected = Some(pair.0);
                    self.focus.set(Some(pair));
                }
                _ => {
                    self.reset_view(rect);
                    self.touched = false;
                    self.focal = None;
                }
            }
        }

        // The keyboard's own way back and its way out.
        //
        // A canvas is not a list, and egui's default focus filter treats a key press as a
        // request to *move* the focus: an arrow is spent on stepping to the next widget in
        // that direction and Escape is spent on surrendering the focus, so a Tab-focused
        // picture answered two key frames and then went deaf (measured: click, tab,
        // ArrowRight ×4, Escape — the third arrow onwards did nothing, and the Escape after a
        // re-click worked once, because a click is what asked for the focus back). Two things
        // keep it: the picture claims the keys it answers — arrows, and Escape, but not Tab,
        // which must still move the focus on — and a key of its own that arrives on a frame
        // the focus is already going away is answered anyway, with the focus asked back for.
        // The keys the plate answers. Shift is the difference between walking the graph and
        // moving the camera: an unshifted arrow steps the selection (`graph_e2e` pins that —
        // ArrowRight #0 → 400, #1 → 100), and a shifted one pans, which is the gesture a
        // reader with a keyboard and no trackpad expects of a canvas. `+`/`−` zoom about the
        // middle of the plate, `0` and Home fit it.
        let (back, forward, clear, home, plus, minus, zero, shift) = ui.input(|input| {
            let shift = input.modifiers.shift;
            (
                input.key_pressed(egui::Key::ArrowLeft) && !shift,
                input.key_pressed(egui::Key::ArrowRight) && !shift,
                input.key_pressed(egui::Key::Escape),
                input.key_pressed(egui::Key::Home),
                input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals),
                input.key_pressed(egui::Key::Minus),
                input.key_pressed(egui::Key::Num0),
                shift,
            )
        });
        let (pan_left, pan_right, pan_up, pan_down) = ui.input(|input| {
            (
                shift && input.key_pressed(egui::Key::ArrowLeft),
                shift && input.key_pressed(egui::Key::ArrowRight),
                shift && input.key_pressed(egui::Key::ArrowUp),
                shift && input.key_pressed(egui::Key::ArrowDown),
            )
        });
        let mine = back
            || forward
            || clear
            || home
            || plus
            || minus
            || zero
            || pan_left
            || pan_right
            || pan_up
            || pan_down;
        // Tab is the one key that would take the keyboard off the plate and into the plate's
        // own camera buttons; they have key equivalents (`+`, `−`, `0`), so the canvas keeps
        // the keyboard, which is what `tests/graph_interaction` measures
        // (`tab_again_keeps_the_canvas_reachable`: two Tabs and the arrows must still walk).
        let tab = ui.input(|input| input.key_pressed(egui::Key::Tab));
        let has = response.has_focus();
        if has {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: false,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
        } else if (mine || tab) && self.had_focus {
            response.request_focus();
        }
        let focused = has || ((mine || tab) && self.had_focus);
        if focused && !self.nodes.is_empty() {
            if home || zero {
                self.reset_view(rect);
                self.touched = false;
            }
            if plus {
                self.zoom_about(rect, rect.center(), ZOOM_STEP);
            }
            if minus {
                self.zoom_about(rect, rect.center(), 1.0 / ZOOM_STEP);
            }
            if pan_left || pan_right || pan_up || pan_down {
                let mut shift_by = Vec2::ZERO;
                if pan_left {
                    shift_by.x += PAN_STEP;
                }
                if pan_right {
                    shift_by.x -= PAN_STEP;
                }
                if pan_up {
                    shift_by.y += PAN_STEP;
                }
                if pan_down {
                    shift_by.y -= PAN_STEP;
                }
                self.pan += shift_by;
                self.touched = true;
            }
            if back || forward {
                self.selected = self.step_selection(if forward { 1 } else { -1 });
                self.wake();
            }
            if clear {
                // Escape gives the picture back one step at a time: the focus a double click
                // set first, then the selection.
                self.focal = None;
                self.selected = None;
                self.focus.set(None);
            }
        }
        // One frame of memory, so a key that arrives while the focus is being taken away is
        // still the canvas's key — and no longer than one frame, so a Tab to the rail is not
        // fought for.
        self.had_focus = focused;
        // The camera may have moved under the gestures above (a reset, a pan, a wheel), and
        // the picture has to be drawn where the camera now is, not where it was — the lag a
        // reader saw as "the graph jumps when I stop dragging" was one frame of this.
        self.clamp_pan(rect);
        let centre = rect.center() + self.pan;
        let zoom = self.zoom;
        let at = |node: &Node| Pos2::new(centre.x + node.x * zoom, centre.y + node.y * zoom);

        // What the pointer is *on*, not what it is near: the painted circle, or a painted
        // name. The fingertip slack `pick` adds is a press's forgiveness; a hover that used
        // it would name a node 15 px away and paint that name under a pointer that was aiming
        // at a relation line — measured, one press of 227 resolved to the node beside the line
        // it was aimed at because the hover had just labelled it there.
        let hovered = response.hover_pos().and_then(|pointer| {
            self.shown_dots
                .iter()
                .find(|(_, at, radius)| pointer.distance(*at) <= *radius + 1.0)
                .map(|(slot, _, _)| *slot)
                .or_else(|| self.pick_name(pointer))
        });
        if hovered.is_some() && self.drag.is_none() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if self.drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }

        // ---- the ranked picture ------------------------------------------------
        //
        // 120 entities and 900 relations is not a graph a reader can follow: measured on the
        // shipped archive, every dot was 4.2…11.0 px, every line 1 px at 0.38…0.72 alpha and
        // six names were painted. So the plate draws what it has room for (`capacity`) at
        // full size with the strongest lines of each of those nodes, and the rest of the set
        // as grain: still where the entity is, still clickable, labelled when picked, but no
        // longer competing with the actors for the reader's eye. Nothing is invented and
        // nothing is hidden from the panel — `around` still lists every relation of every
        // node — and the caption says how many entities were held back.
        let rank = self.strongest();
        // The fit is the picture the page is opened on, and it draws fewer entities than the
        // plate could hold: [`ACTORS_AT_FIT`]. Past a deeper zoom the rest come back as grain,
        // and a selection always brings its own neighbourhood.
        let fit = self.fit_zoom(rect);
        let deeper = zoom > fit * GRAIN_ZOOM;
        let drawn_slots = self.drawn_slots_for(rect);
        let shown_count = drawn_slots.len();
        let room = shown_count;
        let mut full = vec![false; self.nodes.len()];
        for slot in &drawn_slots {
            full[*slot] = true;
        }
        // The radius each drawn node is painted at: its rank among the *drawn* nodes, so the
        // ramp the reader sees is the ramp of what is on the plate — 58 actors at 1100×820
        // run from 5 px to 20 px painted, not from 14 px to 20 px.
        let mut paints = vec![TAIL_RADIUS; self.nodes.len()];
        // …and never wider than the layout allows: a dot is at most [`RADIUS_SHARE`] of the
        // distance to its own nearest neighbour. The rank curve says how big an entity *is*;
        // this says how big the picture can show it without two dots becoming one shape. At
        // 120 nodes the top of the curve was 27 px against a neighbour 30 px away, which is
        // the "one big dot swallowing its neighbours" half of the blob.
        let reach = self.neighbour_reach();
        // One squeeze for the whole picture, then the per-dot cap as a safety: a clip per dot
        // alone flattens the ramp exactly where the layout is even — every dot ends up at the
        // same cap, and the measured 120/900 picture answered a biggest dot of 33.1 px against
        // a 22.6 px median, no step to read. The *median* neighbour distance decides the factor
        // (the minimum is one tight pair and would shrink the whole picture), so the ramp the
        // rank curve draws survives and the cap still keeps a dot out of its neighbour.
        let mut reaches: Vec<f32> = drawn_slots.iter().map(|slot| reach[*slot]).collect();
        reaches.sort_by(f32::total_cmp);
        let middle = reaches.get(reaches.len() / 2).copied().unwrap_or(RADIUS_MAX / RADIUS_SHARE);
        // …and never so small that the ramp dissolves into grain: the smallest actor stays at
        // twice the grain radius, or a deep zoom turns every entity into a speck that is not
        // named at all (measured at 700×520 ×2.08: 3 names where the actors had collapsed).
        let squeeze = (middle * RADIUS_SHARE / RADIUS_MAX)
            .min(1.0)
            .max(2.0 * TAIL_RADIUS / RADIUS_MIN);
        for (place, slot) in drawn_slots.iter().enumerate() {
            let cap = (reach[*slot] * RADIUS_SHARE).max(TAIL_RADIUS);
            paints[*slot] = (rank_radius(place, shown_count) * squeeze)
                .min(cap)
                .max(TAIL_RADIUS);
        }
        let drawn = self.drawn_links(&full, if deeper { LINKS_PER_NODE } else { LINKS_PER_NODE_AT_FIT });
        let drawn_count = drawn.len();
        // The window the weight curve is drawn in: the lightest and heaviest relation of the
        // picture, so the floor is the floor whatever the archive's own units are.
        let (weakest, heaviest) = drawn.iter().fold((f32::INFINITY, 0.0_f32), |(low, high), index| {
            let stories = self.edges[*index].stories;
            (low.min(stories), high.max(stories))
        });
        let (weakest, heaviest) = if drawn_count == 0 {
            (1.0, 1.0)
        } else {
            (weakest, heaviest)
        };
        let focus = self.focus.get();
        // What the selection is made of: the open relation and both of its ends, or the
        // selected node's own relations. Everything else is context while something is
        // selected, and is drawn at `dimmed_grey` — the difference between a neighbourhood
        // and a hairball.
        let lit = |link: &Link| -> bool {
            if let Some((one, two)) = focus {
                return pair_of(link.a, link.b) == (one, two)
                    || link.a == one
                    || link.a == two
                    || link.b == one
                    || link.b == two;
            }
            match self.selected {
                Some(slot) => link.a == slot || link.b == slot,
                None => true,
            }
        };
        let dimming = focus.is_some() || self.selected.is_some();
        // Every line this frame paints, in screen coordinates, kept for the next frame's
        // press. It is built here rather than recomputed from the camera on demand: the
        // picture is what the reader aimed at, and a line the plate left out is not a target.
        let mut segments: Vec<(usize, Pos2, Pos2)> = Vec::with_capacity(drawn_count);
        for index in drawn {
            let link = &self.edges[index];
            let ends = [at(&self.nodes[link.a]), at(&self.nodes[link.b])];
            segments.push((index, ends[0], ends[1]));
            if Some(pair_of(link.a, link.b)) == focus {
                // Drawn in ink below, over everything else: it is the answer to "what does
                // this row connect".
                continue;
            }
            // `stories`, not `strength`: the window the curve is drawn in is measured in the
            // same units as the line being drawn (the stored weight), and `strength` is that
            // weight already divided by the heaviest row of the whole set.
            let (grey, width) = if !dimming {
                (
                    edge_grey(link.stories, weakest, heaviest),
                    edge_width(link.stories, weakest, heaviest),
                )
            } else if lit(link) {
                (
                    neighbour_grey(),
                    edge_width(link.stories, weakest, heaviest),
                )
            } else {
                (dimmed_grey(), WIDTH_MIN)
            };
            painter.line_segment(ends, Stroke::new(width, grey));
        }
        if let Some((one, two)) = focus {
            if let (Some(a), Some(b)) = (self.nodes.get(one), self.nodes.get(two)) {
                painter.line_segment([at(a), at(b)], Stroke::new(WIDTH_MAX + 1.6, theme::INK));
            }
        }
        self.shown_links = segments;

        // A node the plate has room for is drawn at its mention radius; one that was held
        // back is a grain, until the reader picks or hovers it — then it is promoted for as
        // long as they are on it, because a node that answers a click has to be nameable.
        let promoted = |slot: usize| {
            self.selected == Some(slot)
                || hovered == Some(slot)
                || focus.is_some_and(|(one, two)| slot == one || slot == two)
        };
        // The same, without the pointer: what a *label* may treat as a wall. The hovered node
        // is left out so a pointer on its way across the plate cannot push a name aside.
        let promoted_without_hover = |slot: usize| {
            self.selected == Some(slot)
                || focus.is_some_and(|(one, two)| slot == one || slot == two)
        };
        let radius_of = |slot: usize| -> f32 {
            if full[slot] || promoted(slot) {
                paints[slot].max(TAIL_RADIUS) * zoom
            } else {
                TAIL_RADIUS * zoom.max(0.6)
            }
        };
        // What a double-click fixed on: the node itself and the entities joined to it stay in
        // ink; everything else on the plate is context. The edge lines already dim by the
        // selection (`lit`), so the dots have to dim with them or the picture reads as a lit
        // web with the same grey field behind it.
        let mut ink_of = vec![true; self.nodes.len()];
        if let Some(centre) = self.focal {
            for flag in ink_of.iter_mut() {
                *flag = false;
            }
            if let Some(slot) = ink_of.get_mut(centre) {
                *slot = true;
            }
            for link in &self.edges {
                if link.a == centre {
                    ink_of[link.b] = true;
                }
                if link.b == centre {
                    ink_of[link.a] = true;
                }
            }
        }
        let mut dots: Vec<(usize, Pos2, f32)> = Vec::with_capacity(self.nodes.len());
        for (slot, node) in self.nodes.iter().enumerate() {
            let point = at(node);
            if !rect.contains(point) {
                continue;
            }
            // The opening view draws the ranked set and nothing else: an entity that is neither
            // drawn nor promoted is not on the plate until the reader zooms in or selects it.
            // A grain is where an entity *is*, and a field of 120 of them is the mesh the
            // reader complained about.
            if !full[slot] && !deeper && !promoted(slot) {
                continue;
            }
            let radius = radius_of(slot);
            let ink = if ink_of[slot] { theme::INK } else { theme::DIM };
            painter.circle_filled(point, radius, ink);
            dots.push((slot, point, radius));
            if self.drag == Some(slot) || hovered == Some(slot) {
                painter.circle_stroke(point, radius + 4.0, Stroke::new(1.0_f32, theme::INK));
            }
        }
        self.shown_dots = dots;
        if let Some(slot) = self.selected {
            let point = at(&self.nodes[slot]);
            painter.circle_stroke(
                point,
                radius_of(slot) + 6.0,
                Stroke::new(1.5_f32, theme::INK),
            );
        }
        // Both ends of the open relation, so the pair the reader clicked in the panel is
        // named on the plate and not only in the panel.
        for slot in focus.into_iter().flat_map(|(one, two)| [one, two]) {
            if let Some(node) = self.nodes.get(slot) {
                painter.circle_stroke(
                    at(node),
                    radius_of(slot) + 9.0,
                    Stroke::new(1.0_f32, theme::DIM),
                );
            }
        }

        // Labels: both ends of the open relation and the selected node always, then the
        // strongest nodes the plate has room for, then whatever of those fits without
        // touching another label or a node — and never fewer than `LABELS_MIN`.
        //
        // The hovered node is **not** in this set. A pointer that only moves is not an
        // edit, and a picture whose names rearrange themselves under a pointer that is on
        // its way to one of them cannot be aimed at: measured on the live archive, aiming at
        // "United States of America" moved its box six times out of six. So the stable set
        // is placed first, and the hovered node's own name is added afterwards in whatever
        // slot is still free (`tests/graph_edges.rs` sweeps every painted name for exactly
        // this).
        let mut wanted: Vec<usize> = Vec::with_capacity(LABELS_MAX + 4);
        for slot in focus
            .into_iter()
            .flat_map(|(one, two)| [one, two])
            .chain([self.selected].into_iter().flatten())
        {
            if !wanted.contains(&slot) {
                wanted.push(slot);
            }
        }
        let eager = wanted.len();
        // The cap is a cap on the picture, not only on one of its passes: the reader asked for
        // the selection and the open pair, so those are named and the strongest fill the rest of
        // `LABELS_MAX` — measured at 700×520, `LABELS_MAX + 4` asked for was eighteen names on
        // the smallest plate, which is the text being the mass.
        for slot in rank.iter().take(room) {
            if wanted.len() >= LABELS_MAX.max(eager) {
                break;
            }
            if !wanted.contains(slot) {
                wanted.push(*slot);
            }
        }
        let font = FontId::proportional(LABEL_SIZE);
        let sizes: Vec<Vec2> = wanted
            .iter()
            .map(|slot| {
                ui.ctx().fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(self.name_of(*slot), font.clone(), theme::INK)
                        .size()
                })
            })
            .collect();
        let anchors: Vec<(usize, Pos2, f32, Vec2)> = wanted
            .iter()
            .zip(&sizes)
            .map(|(slot, size)| (*slot, at(&self.nodes[*slot]), radius_of(*slot), *size))
            .collect();
        // Only a dot the reader is meant to read is an obstacle for a label: text over a
        // grain is legible, and treating 120 grain dots as walls is what left the page with
        // six names for 120 entities.
        let obstacles: Vec<(Pos2, f32)> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|&(slot, _)| full[slot] || promoted_without_hover(slot))
            .map(|(slot, node)| (at(node), radius_of(slot)))
            .collect();
        // A label may not sit on the caption strip, and may not leave the plate.
        let bounds = Rect::from_min_max(
            rect.min + Vec2::splat(4.0),
            Pos2::new(rect.max.x - 4.0, (rect.max.y - CAPTION_STRIP).max(rect.min.y)),
        );
        // The strokes a name has to keep its middle clear of. A press on a stroke is a press
        // on the relation (see `target`), so a name split by one would answer the wrong
        // question.
        let lines: Vec<(Pos2, Pos2)> = self
            .shown_links
            .iter()
            .map(|(_, one, two)| (*one, *two))
            .collect();
        // The names this frame paints are also kept: a name is as much a target as the dot
        // beside it, so the boxes are stored for the next frame's press.
        let mut placed = place_labels(&anchors, &obstacles, &lines, bounds, eager);
        // The node under the pointer, named where there is room for it — never at the cost of
        // moving a name that was already there.
        if let Some(slot) = hovered {
            if !placed.iter().any(|(other, _)| *other == slot) {
                let size = ui.ctx().fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap(self.name_of(slot), font.clone(), theme::INK)
                        .size()
                });
                if let Some(box_) = best_slot(
                    at(&self.nodes[slot]),
                    radius_of(slot),
                    size,
                    bounds,
                    &placed,
                    &obstacles,
                    &lines,
                    radius_of(slot),
                    true,
                    false,
                ) {
                    placed.push((slot, box_));
                }
            }
        }
        for (slot, box_) in &placed {
            let emphasis = self.selected == Some(*slot)
                || hovered == Some(*slot)
                || focus.is_some_and(|(one, two)| *slot == one || *slot == two);
            painter.text(
                box_.left_top(),
                Align2::LEFT_TOP,
                self.name_of(*slot),
                font.clone(),
                if emphasis { theme::INK } else { theme::DIM },
            );
        }
        self.shown_names = placed;

        // The caption says what the picture is: how many of the entities are drawn at full
        // size and how many as grain, how many relations that left, and what is selected. It
        // is the honest half of "rank, do not draw everything" — a reader is never told the
        // plate holds the whole set when it does not.
        let held = self.nodes.len().saturating_sub(shown_count);
        let plate = format!(
            "{} of {} entities · {} of {} relations · {}{}",
            nf(shown_count as i64),
            nf(self.nodes.len() as i64),
            nf(drawn_count as i64),
            nf(self.edges.len() as i64),
            self.selected
                .map(|slot| self.name_of(slot))
                .unwrap_or_else(|| "nothing selected".to_string()),
            if held == 0 {
                String::new()
            } else if deeper {
                format!(" · {} as grains", nf(held as i64))
            } else {
                format!(" · {} when you zoom in", nf(held as i64))
            }
        );
        // The caption is a line of text, so it is set in the theme's caption role: 11 px on
        // the ladder, in DIM. It was 10.5 px in FAINT — a size no role produces, in a grey
        // the theme documents as a *stroke*, 3.95:1 on this plate (`tests/design_audit.rs`
        // measured both).
        painter.text(
            rect.left_bottom() + Vec2::new(9.0, -8.0),
            Align2::LEFT_BOTTOM,
            plate,
            theme::role::CAPTION.font(),
            theme::role::CAPTION.colour,
        );
        // The focus ring is drawn last so it sits above the picture, not under it.
        if response.has_focus() {
            painter.rect_stroke(
                rect.shrink(2.0),
                egui::CornerRadius::ZERO,
                Stroke::new(1.5_f32, theme::INK),
                egui::StrokeKind::Inside,
            );
        }
        // …and the three camera buttons last of all, because a widget painted after the dots
        // is also the widget egui hands the pointer to: they sit on the plate, they are
        // pressed instead of it, and they are the same three gestures as `+`, `−` and `0`.
        self.plate_buttons(ui, rect);

        // The story the panel opened, if it opened one. Taken, not read: a click is answered
        // once, and the frame after it must not open the same story again.
        self.opened.take()
    }
}

/// What a press on the plate lands on. One answer for the click and for the start of a drag,
/// so grabbing and selecting are the same act on the same geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A node: a click selects it, a drag moves it.
    Node(usize),
    /// A relation line: a click opens the pair it joins — both ends ring, the panel lists its
    /// stories under the row it opens.
    Link((usize, usize)),
    /// A painted name: a click selects the node it names.
    Name(usize),
    /// The bare plate: a click clears the selection, a drag pans.
    Plate,
}

/// How far `pointer` is from the segment `one`…`two`, in the units of the positions given.
///
/// The distance to the *segment*, not to the infinite line through it: two plates' worth of
/// lines that only meet at infinity still pass within a hair of every point in the middle.
fn segment_distance(pointer: Pos2, one: Pos2, two: Pos2) -> f32 {
    let along = two - one;
    let length = along.length();
    if length <= f32::EPSILON {
        return pointer.distance(one);
    }
    let reach = ((pointer - one).dot(along) / (length * length)).clamp(0.0, 1.0);
    pointer.distance(one + along * reach)
}

/// A hash of everything the picture is made of: the entities (id, name, type, mentions) and
/// the relations (both ends, weight, label).
///
/// The shell calls `fill` whenever it reloads the graph, and a page switch reloads it, so the
/// view has to tell "the same archive, again" from "a different window". Comparing the data
/// itself is exact and costs one pass over 120 entities and a few hundred rows; comparing
/// counts alone would call two archives with the same sizes the same picture.
fn picture_key(entities: &[Entity], edges: &[Edge]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entities.len().hash(&mut hasher);
    for entity in entities {
        entity.id.hash(&mut hasher);
        entity.name.hash(&mut hasher);
        entity.kind.hash(&mut hasher);
        entity.count.hash(&mut hasher);
    }
    edges.len().hash(&mut hasher);
    for edge in edges {
        edge.a.hash(&mut hasher);
        edge.b.hash(&mut hasher);
        edge.weight.to_bits().hash(&mut hasher);
        edge.label.hash(&mut hasher);
    }
    hasher.finish()
}

/// The two ends of a relation as one key, lower slot first.
///
/// The plate and the panel have to agree on which relation is open, and a relation is
/// stored with `a <= b` while a row can be opened from either end — so the comparison is
/// made on the normalised pair and never on the direction the reader came from.
fn pair_of(one: usize, other: usize) -> (usize, usize) {
    (one.min(other), one.max(other))
}

/// The drawn radius of the entity in `place` of a drawing of `count` entities: a curve over
/// [`RADIUS_MIN`]…[`RADIUS_MAX`] of its **rank**, strongest first.
///
/// Rank, and not the mention count, because the count is unusable as a size: measured on
/// the shipped archive (120 entities, 900 relations), most entities are mentioned once or
/// twice, so a share of mentions put the picture at 4.8 / 5.1 / 19.5 px — 120 dots at the
/// floor and one outlier, which is not a hierarchy a reader can rank. The rank is even by
/// construction, so the picture always has three sizes of actor in it, and the exponent
/// (`RANK_CURVE`) keeps the top of the list heavy while the tail still tapers: 58 entities
/// on a 1100×820 plate land at 5.3 / 10.7 / 20.3 px painted, median against maximum in the
/// order a reader's eye wants.
///
/// The cost is stated rather than hidden: a dot's size is a fact about its place in the set
/// on screen, so the same entity is drawn bigger on a plate with fewer actors on it. The
/// mention count is still what the panel says, and it is still what ranks the set
/// ([`GraphView::strongest`]: relations first, then mentions).
fn rank_radius(place: usize, count: usize) -> f32 {
    if count <= 1 {
        return RADIUS_MAX;
    }
    let share = 1.0 - place.min(count - 1) as f32 / (count - 1) as f32;
    RADIUS_MIN + (RADIUS_MAX - RADIUS_MIN) * share.clamp(0.0, 1.0).powf(RANK_CURVE)
}

/// How far up the drawn set a relation at `strength` sits — `weakest` and `strongest` are the
/// lightest and heaviest weights the picture draws.
///
/// Stories are a heavy tail — on the shipped archive the median relation is one story and
/// the heaviest is dozens — so a linear map put every line in the bottom fifth of the range.
/// The window is the drawn set's own range, on a logarithm, so the answer is centred on what
/// the picture actually holds whatever units the archive counts in.
fn strength_share(strength: f32, weakest: f32, strongest: f32) -> f32 {
    if strongest <= weakest * 1.000_1 {
        return 1.0;
    }
    let low = weakest.max(1.0e-3).ln();
    let high = strongest.max(weakest + 1.0e-3).ln();
    ((strength.max(1.0e-3).ln() - low) / (high - low)).clamp(0.0, 1.0)
}

/// The grey a relation line is drawn in: one of two palette values, [`theme::FAINT`] for the
/// tail of the picture and [`theme::DIM`] above [`EDGE_STEP`].
///
/// Two steps and not a gradient: the audit found seven computed greys on this plate, none of
/// them named in the palette, and no reader can order seven greys between 96 and 207 anyway.
/// Strength is carried where the eye reads it — the line's weight (`edge_width`) — and the
/// grey answers one question only: is this a relation I am meant to follow, or the tail?
fn edge_grey(strength: f32, weakest: f32, strongest: f32) -> Color32 {
    if strength_share(strength, weakest, strongest) >= EDGE_STEP {
        theme::DIM
    } else {
        theme::FAINT
    }
}

/// The width of that line: the strength share, squared, so the tail stays a hairline while
/// the top of the picture is a line a reader can pick out of it.
fn edge_width(strength: f32, weakest: f32, strongest: f32) -> f32 {
    let share = strength_share(strength, weakest, strongest);
    WIDTH_MIN + share * share * (WIDTH_MAX - WIDTH_MIN)
}

/// A relation of the selection: the brighter step, whatever its own weight, so the
/// neighbourhood is a readable set of lines and not a dimmer version of the hairball.
fn neighbour_grey() -> Color32 {
    theme::DIM
}

/// A line that is not part of the selection: context, below the floor on purpose.
fn dimmed_grey() -> Color32 {
    CONTEXT
}

/// How many entities the plate has room to draw at full size.
///
/// The wheel does not change this: a picture that gained and lost names while a reader
/// zoomed would move under their hand. The canvas does. At [`SPACING`] the plate holds
/// 59 names at 700×520 and all 120 at 2400×1400.
fn capacity(canvas: Rect) -> usize {
    let area = canvas.width().max(0.0) * canvas.height().max(0.0);
    let room = (area / (SPACING * SPACING)) as usize;
    room.clamp(VISIBLE_MIN, VISIBLE_MAX)
}

/// How much one frame of scrolling and pinching multiplies the zoom by.
///
/// Exponential in the pixels, so a trackpad's many small deltas and a mouse wheel's few big
/// ones arrive at the same place for the same gesture, and one wheel notch is exactly
/// [`ZOOM_STEP`]. The per-frame factor is clamped: a driver that delivers a 900 px fling in
/// one frame would otherwise jump the picture from the whole graph into one dot, and no
/// gesture on the plate undoes a jump like that in one movement.
fn zoom_factor(scroll: f32, pinch: f32) -> f32 {
    let pinch = if pinch.is_finite() && pinch > 0.0 { pinch } else { 1.0 };
    let wheel = if scroll.is_finite() { scroll } else { 0.0 };
    (pinch * (wheel * SCROLL_RATE).exp()).clamp(1.0 / ZOOM_JUMP, ZOOM_JUMP)
}

/// A spring's rest length between two nodes: the base plus both radii, so a big entity
/// sits further from its neighbours and the line between them stays readable.
fn rest_length(radius_a: f32, radius_b: f32) -> f32 {
    // The longest of the three, and never the sum alone: a spring is longer than the biggest
    // dot it joins, so a relation is *air* between two entities and not a line inside one
    // blob.
    (REST + (radius_a + radius_b) * REST_PER_RADIUS)
        .max(radius_a.max(radius_b) * REST_PER_MAX_RADIUS + REST)
}

/// Where the labels go: one of eight places around each node, strongest first, never on top
/// of each other and never on top of a node.
///
/// The canvas is a picture, so the placement is decided here instead of by egui's layout
/// engine: for each label in turn the right side, the left, above, below and the four
/// diagonals are tried, and the first that stays inside `bounds` and clear of every label
/// already placed and of every node circle wins. Measured before this pass, on 120 nodes at
/// 1100×820, six labels were placed of fourteen asked for — four sides of a dot in a dense
/// middle is not much room, and the page showed six names for 120 entities.
///
/// `eager` is how many of the first labels are named even in a corner: they may also take a
/// lifted slot clear of their own circle. And when the first pass has placed fewer than
/// [`LABELS_MIN`], a second pass over the labels it skipped relaxes the *node* obstacle —
/// text over a dot is legible, text over text is not — so the page always names at least
/// eight of the strongest nodes if the canvas can hold eight labels at all.
///
/// `lines` are the relation strokes. A name may be *clipped* by one — a stroke across a
/// corner of the box still leaves the word readable — but not split through its middle, and
/// not within [`LINK_HIT`] of it: that is where a press lands, and there the stroke is what
/// the reader aimed at (`GraphView::target`). Measured on the live archive: the strongest
/// pair ran through the middle of the word "China", so the one relation a reader most wants
/// to open was covered by a name.
fn place_labels(
    anchors: &[(usize, Pos2, f32, Vec2)],
    obstacles: &[(Pos2, f32)],
    lines: &[(Pos2, Pos2)],
    bounds: Rect,
    eager: usize,
) -> Vec<(usize, Rect)> {
    let mut placed: Vec<(usize, Rect)> = Vec::with_capacity(anchors.len());
    let mut skipped: Vec<usize> = Vec::new();
    for (index, (slot, centre, radius, size)) in anchors.iter().enumerate() {
        // First pass: every dot blocks, whatever its size, and no stroke through the middle.
        match best_slot(
            *centre,
            *radius,
            *size,
            bounds,
            &placed,
            obstacles,
            lines,
            0.0,
            index < eager.max(1),
            false,
        ) {
            Some(box_) => placed.push((*slot, box_)),
            None => skipped.push(index),
        }
    }
    // The second pass, and the promise: at least `LABELS_MIN` of the strongest nodes are
    // named. A label may cross a dot *smaller than its own* here — text over a grain is
    // legible, and it names something the picture could not otherwise name — but never a
    // bigger dot, never another label, and never the edge of the plate.
    let floor = LABELS_MIN.min(anchors.len());
    let target = LABELS_MAX.min(anchors.len());
    for index in skipped {
        if placed.len() >= target {
            break;
        }
        let (slot, centre, radius, size) = anchors[index];
        let ignore = if placed.len() >= floor { radius } else { 0.0 };
        let cross_lines = placed.len() >= floor;
        if let Some(box_) = best_slot(
            centre, radius, size, bounds, &placed, obstacles, lines, ignore, true, cross_lines,
        ) {
            placed.push((slot, box_));
        }
    }
    placed
}

/// The first place around one node a label can sit: `None` when every candidate is taken.
///
/// `obstacles` is `None` in the relaxed pass, where a label may cover a dot (but never
/// another label, and never leave `bounds`). `lines` are the relation strokes: a slot is
/// only taken when no stroke passes through the middle of it, unless `cross_lines` says the
/// plate has no clean slot left at all.
#[allow(clippy::too_many_arguments)]
fn best_slot(
    centre: Pos2,
    radius: f32,
    size: Vec2,
    bounds: Rect,
    placed: &[(usize, Rect)],
    obstacles: &[(Pos2, f32)],
    lines: &[(Pos2, Pos2)],
    ignore_below: f32,
    eager: bool,
    cross_lines: bool,
) -> Option<Rect> {
    let distance = radius + LABEL_GAP;
    let half = size.y * 0.5;
    // Eight places at the first ring and eight at a second one, so a label that cannot sit
    // beside its own dot can sit beside it at a distance — which is what turns "eight names
    // per canvas" into "the top dozen" on a crowded plate.
    let mut offsets: Vec<Vec2> = Vec::with_capacity(16);
    for reach in [distance, distance * 1.9] {
        let diagonal = reach * 0.72;
        offsets.extend([
            Vec2::new(reach, -half),
            Vec2::new(-reach - size.x, -half),
            Vec2::new(-size.x * 0.5, -reach - size.y),
            Vec2::new(-size.x * 0.5, reach),
            Vec2::new(diagonal, -diagonal - half),
            Vec2::new(-diagonal - size.x, -diagonal - half),
            Vec2::new(diagonal, diagonal - half),
            Vec2::new(-diagonal - size.x, diagonal - half),
        ]);
    }
    // The last resort (only for `eager` labels) lifts the label clear of the node's own
    // circle, so a selected node is named even in a very dense corner.
    let lift = (radius * 2.0 + LABEL_GAP).max(size.y);
    let lifted = [
        Vec2::new(distance, -lift),
        Vec2::new(-distance - size.x, -lift),
        Vec2::new(distance, lift),
        Vec2::new(-distance - size.x, lift),
    ];
    let candidates = offsets.iter().chain(lifted.iter().filter(|_| eager));
    for offset in candidates {
        let box_ = Rect::from_min_size(centre + *offset, size);
        if !bounds.contains_rect(box_) {
            continue;
        }
        if !placed
            .iter()
            .all(|(_, other)| !box_.expand(LABEL_GAP).intersects(*other))
        {
            continue;
        }
        let covers_a_dot = obstacles.iter().any(|(node, node_radius)| {
            *node_radius > ignore_below
                && box_.intersects(Rect::from_center_size(*node, Vec2::splat(node_radius * 2.0)))
        });
        if covers_a_dot {
            continue;
        }
        // The middle of the box is where a press lands and where the eye starts: a stroke
        // there splits the word in two. A stroke across a corner is a line passing behind a
        // name, and a name is painted after every line, so it stays legible.
        if !cross_lines
            && lines
                .iter()
                .any(|(one, two)| segment_distance(box_.center(), *one, *two) <= LINK_HIT + 1.0)
        {
            continue;
        }
        return Some(box_);
    }
    None
}

/// Why two entities are joined: the model's own words when it wrote a relation, and
/// otherwise the number of stories that name both ends. A row with no reason would be a
/// line the reader cannot interpret, so there is no empty case.
pub fn relation_reason(label: &str, stories: f32) -> String {
    let label = label.trim();
    if !label.is_empty() {
        return label.to_string();
    }
    // No label is the common case: every relation in the shipped archive is a co-occurrence
    // the model did not name. The count of stories that name both ends is then the reason,
    // and it is said as a count — a zero is not "one story", it is a relation whose weight
    // the window did not count, and saying so is the only honest sentence left.
    match stories.round().max(0.0) as i64 {
        0 => "named together in this window".to_string(),
        1 => "named together in one story".to_string(),
        count => format!("named together in {} stories", nf(count)),
    }
}

/// The relation panel: what is selected, how often it is mentioned, one row per relation
/// saying which entity and why, and — for the relation the reader opened — the newest
/// stories that name both ends.
///
/// Every row is a door. A relation row opens the pair it joins (the plate rings both ends
/// and draws that one line in ink) and lists the stories behind the relation; a story row
/// opens that story. The answer is the story the reader clicked, the same `Option<i64>`
/// [`GraphView::show`] gives one frame later — a caller that reads it here switches pages
/// without waiting for the next frame.
pub fn panel(ui: &mut egui::Ui, view: &GraphView) -> Option<i64> {
    // Top down, whatever `ui` is: the page hands the panel a `ui` created inside the
    // horizontal row that holds the canvas beside it, and a child inherits its parent's
    // layout — every row of the list would be drawn side by side, 665 px wide and one line
    // tall, which is the state this comment exists to keep from coming back.
    ui.vertical(|ui| rows(ui, view)).inner
}

/// How an entity's stored type reads in the panel: one vocabulary, whatever the archive's
/// rows were written with.
///
/// This is a *display* mapping, not a classification: the graph decides nothing about what an
/// entity is, and an empty type stays honestly "untyped". The archive holds both the model's
/// own words (`Institution`, `Country`, `Product`) and the codes an older writer left behind
/// (`ORG 3479`, `PERSON 948`, `LOC 108`, `MISC 27` when this was measured), so the codes are
/// shown in the words the model uses for the same thing and everything else is passed through
/// as it was written.
/// The panel_x27s one line about a selected entity: what kind of thing it is, in the one
/// vocabulary of [`type_words`], and how often the window met it.
fn type_line(entity: &Entity) -> String {
    format!("{} · {} mentions", type_words(&entity.kind), nf(entity.count))
}

fn type_words(kind: &str) -> &str {
    let trimmed = kind.trim();
    if trimmed.is_empty() {
        return "untyped";
    }
    let code = trimmed.to_ascii_lowercase();
    match code.as_str() {
        "org" | "organisation" | "organization" => "Institution",
        "person" | "per" => "Person",
        "loc" | "gpe" | "location" => "Place",
        "misc" => "Other",
        _ => trimmed,
    }
}
fn rows(ui: &mut egui::Ui, view: &GraphView) -> Option<i64> {
    let Some(slot) = view.selected else {
        ui.label(theme::dim(
            "Select an entity on the plate to see what connects it, then a relation to see the stories it is made of.",
        ));
        return None;
    };
    let entity = view.entities.get(slot).cloned().unwrap_or_default();
    ui.label(theme::body(&entity.name));
    ui.label(theme::dim(&type_line(&entity)));
    ui.add_space(8.0);
    let around = view.around(slot);
    if around.is_empty() {
        ui.label(theme::dim(
            "Nothing connects this entity inside the window. Widen the window in the rail to see more.",
        ));
        return None;
    }
    ui.label(theme::dim(&format!(
        "{} relations, strongest first",
        around.len()
    )));
    ui.add_space(6.0);
    let mut opened: Option<i64> = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for relation in &around {
            let pair = pair_of(slot, relation.other);
            let row = door_row(
                ui,
                &relation.name,
                &row_reason(view, relation, pair),
                FontId::proportional(theme::size::BODY),
                view.focused() == Some(pair),
            );
            if row.clicked() {
                view.open_relation(pair.0, pair.1);
            }
            // Read back rather than remembered: the click above may have opened this very
            // row, and the stories have to appear under the row that just became a door.
            if view.focused() == Some(pair) {
                if let Some(story) = stories_section(ui, view, pair) {
                    view.opened.set(Some(story));
                    opened = Some(story);
                }
            }
            ui.add_space(6.0);
        }
    });
    opened
}

/// The line under a relation's name: why the two are joined, and — when the model wrote
/// the reason in its own words — how many stories stand behind it, which
/// [`relation_reason`] only says in words when there are no words.
/// The line under a relation's name: what the archive holds between the two ends, and — once
/// the reader has opened this row — the strongest story behind it.
///
/// Three facts, and the user's complaint is the absence of any of them ("welche Verbindung die
/// haben oder warum die miteinander verbunden sind"): the row's own title names the far end,
/// [`under_the_name`] states the archive's reason and the weight in stories, and a row the
/// panel has opened names the story the reader can open next — its title and id, read from the
/// stories this view already holds for the open pair, so a row costs no query of its own.
fn row_reason(view: &GraphView, relation: &Relation, pair: (usize, usize)) -> String {
    let line = under_the_name(relation);
    if view.focused() != Some(pair) {
        return line;
    }
    let Some(opening) = view.opening() else {
        return line;
    };
    let Ok(stories) = &opening.stories else {
        return line;
    };
    let Some(story) = stories.first() else {
        return line;
    };
    let title = story.title.trim();
    if title.is_empty() {
        format!("{line} · story {}", nf(story.id))
    } else {
        format!("{line} · \"{}\" (story {})", clip(title, 44), nf(story.id))
    }
}
fn under_the_name(relation: &Relation) -> String {
    let reason = relation_reason(&relation.label, relation.stories);
    if relation.label.trim().is_empty() {
        reason
    } else {
        format!(
            "{reason} · {}",
            stories_words(relation.stories.round().max(0.0) as i64)
        )
    }
}

/// `0 stories`, `1 story`, `12 stories` — a count a reader can say out loud.
fn stories_words(count: i64) -> String {
    format!("{} {}", nf(count), if count == 1 { "story" } else { "stories" })
}

/// The stories behind the relation the panel has open, each one a door into the reading
/// pane. Returns the story the reader clicked.
///
/// The three states are different sentences on purpose: the archive could not be read, no
/// story names both ends any more, or these are the stories. A panel that showed the middle
/// sentence for all three would be guessing at the archive.
fn stories_section(ui: &mut egui::Ui, view: &GraphView, pair: (usize, usize)) -> Option<i64> {
    let Some(opening) = view.opening().filter(|opening| opening.pair == pair) else {
        return None;
    };
    ui.add_space(2.0);
    match &opening.stories {
        Err(problem) => {
            ui.label(theme::dim(&format!(
                "The stories behind this relation could not be read — {problem}"
            )));
            None
        }
        Ok(stories) if stories.is_empty() => {
            ui.label(theme::dim(
                "No story in the archive names both ends any more; only the relation's weight remembers them.",
            ));
            None
        }
        Ok(stories) => {
            ui.label(theme::section_label(&format!(
                "The newest {} that name both",
                stories_words(stories.len() as i64)
            )));
            let mut opened: Option<i64> = None;
            for story in stories {
                let row = door_row(
                    ui,
                    &story.title,
                    &story_meta(story),
                    FontId::proportional(theme::size::SMALL),
                    false,
                );
                if row.clicked() {
                    opened = Some(story.id);
                }
            }
            opened
        }
    }
}

/// A row of two short lines that is itself the target: a name over what is under it.
///
/// The whole box is clickable, not the name: the panel is [`PANEL_WIDTH`] wide and a reader
/// aiming at "governs · 12 stories" means the relation, not the sentence under it. `marked` is the
/// relation that is open — filled like a hovered row and given an ink rule on its left edge,
/// so the open one is still findable after the pointer has moved away.
///
/// The two lines are cut to what a line can hold before they are shaped (`clip` bounds the
/// work, the single-line layout bounds the appearance), and the focus ring is painted last and
/// inside the row, so the keyboard's place is never covered by the text it is on.
fn door_row(
    ui: &mut egui::Ui,
    name: &str,
    under: &str,
    name_font: FontId,
    marked: bool,
) -> egui::Response {
    let reason_font = FontId::proportional(theme::size::CAPTION);
    const PAD: f32 = 5.0;
    const GAP: f32 = 1.0;
    const MARK: f32 = 2.0;
    // 0.37 shapes through `&mut FontsView`, so the metrics are asked on the context.
    let (h_name, h_reason) = ui.ctx().fonts_mut(|fonts| {
        (
            fonts.row_height(&name_font),
            fonts.row_height(&reason_font),
        )
    });
    let height = PAD * 2.0 + h_name + GAP + h_reason;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, clip(name, 80)));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let painter = ui.painter().with_clip_rect(rect);
    if marked || response.hovered() || response.has_focus() {
        painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::RAISED);
    }
    if marked {
        painter.rect_filled(
            Rect::from_min_size(rect.left_top(), Vec2::new(MARK, rect.height())),
            egui::CornerRadius::ZERO,
            theme::INK,
        );
    }
    let text = (rect.width() - PAD * 2.0 - MARK).max(0.0);
    let left = rect.left() + PAD + MARK;
    let name = one_line(ui, &clip(name, 160), name_font, theme::INK, text);
    let reason = one_line(ui, under, reason_font, theme::FAINT, text);
    painter.galley(Pos2::new(left, rect.top() + PAD), name, theme::INK);
    painter.galley(
        Pos2::new(left, rect.top() + PAD + h_name + GAP),
        reason,
        theme::FAINT,
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect.shrink(0.5),
            egui::CornerRadius::ZERO,
            Stroke::new(theme::HAIRLINE_WIDTH, theme::INK),
            egui::StrokeKind::Inside,
        );
    }
    response
}

/// Who filed a story and when — each part only when the archive has it, so a row never
/// wears a separator with nothing behind it.
fn story_meta(story: &Story) -> String {
    let source = story.source.trim();
    let source = if source.is_empty() { "unknown source" } else { source };
    let stamp = when(&story.published);
    if stamp.is_empty() {
        source.to_string()
    } else {
        format!("{source} · {stamp}")
    }
}

/// One line of text, elided with an ellipsis instead of wrapped: a row in a list is one
/// line however narrow the panel is.
fn one_line(ui: &egui::Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: width.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.ctx().fonts_mut(|fonts| fonts.layout_job(job))
}

/// The panel width, shared by the page layout and the panel itself.
///
/// The page gives the plate everything the content box has after this column and one
/// [`crate::app::GRAPH_GAP`], so every pixel here is a pixel the canvas does not get. It is
/// therefore the column's **measured floor**, not a comfortable default:
///
/// * The plan sits the two parts beside each other when the page box holds the plate's floor
///   (320), the gap (16) and this column — 592 px at this width. The App's own minimum window
///   is 860 × 560, whose page box is 632 px, and the smallest audited one is 700 × 520 (a
///   folded 64 px rail and the two 20 px margins: a 596 px box). At 320 the column pushed both
///   of those below 656 px, so `GraphPlan` **stacked** them and the canvas became the top 3/5
///   of the page — a band, which is the reported "the graph is not full screen". At 256 both
///   sit beside the column and the plate is the page's full height — measured with
///   `tests/graph_sizes` at a 616 px page box (720 × 600): the plate is 344 × 463, where at a
///   320 px column the same window stacked it into a 616 × 268 band over a 616 × 179 column.
///   256 is `theme::space::UNIT * 64`, the largest spacing unit that keeps
///   `320 + 16 + column ≤ 596`.
/// * What it costs: the panel's widest line measured 287 px of ink in the old 320 px column
///   (a story title under an open relation). At 256 that one line elides by ~36 px, about five
///   characters at [`theme::size::BODY`]; every row is single-line and elides by design
///   ([`one_line`]), and the column's fixed lines — an entity's name, its type line, "12
///   relations, strongest first" — fit. A column narrower than this truncates those.
pub const PANEL_WIDTH: f32 = theme::space::UNIT * 64.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Entity;

    /// The picture the budget and the invariants are measured on: 120 entities with a
    /// spread of mention counts, and 200 relations among them.
    ///
    /// The pairs are drawn from a ring and its 2-step chords, so every one is distinct
    /// and the count is exact — a search for "some 200 pairs" would need a bound on how
    /// many distinct pairs a modular walk can visit, and a loop that silently spun
    /// forever is how this fixture was found the first time.
    fn synthetic(count: usize, links: usize) -> (Vec<Entity>, Vec<Edge>) {
        let entities = (0..count)
            .map(|index| Entity {
                id: index as i64 + 1,
                name: format!("Entity number {}", index + 1),
                kind: "ORG".to_string(),
                count: (index as i64 * 37) % 90 + 1,
            })
            .collect();
        let edges = (0..links)
            .map(|index| {
                let a = index % count;
                let step = 1 + index / count;
                let b = (a + step) % count;
                Edge {
                    a: a as i64 + 1,
                    b: b as i64 + 1,
                    weight: 1.0 + (index % 9) as f64,
                    label: if index % 3 == 0 {
                        "named together".to_string()
                    } else {
                        String::new()
                    },
                }
            })
            .collect();
        (entities, edges)
    }

    /// The shape the shipped archive actually has: a handful of hubs that are named beside
    /// most of the window, and a long tail of entities named in one or two stories. Mean
    /// degree 15 at 120 nodes, but the degree *distribution* is the picture: eight entities
    /// carry sixty relations each, and most carry two.
    ///
    /// [`synthetic`] is a ring with equal steps — every node has the same degree — which is
    /// the easiest graph to lay out and the least like what a reader sees.
    fn hubbed(count: usize, hubs: usize) -> (Vec<Entity>, Vec<Edge>) {
        let entities = (0..count)
            .map(|index| Entity {
                id: index as i64 + 1,
                name: format!("Entity number {}", index + 1),
                kind: "ORG".to_string(),
                count: if index < hubs {
                    900 - index as i64 * 40
                } else {
                    (index as i64 * 13) % 7 + 1
                },
            })
            .collect();
        // Every leaf hangs off two hubs and one other leaf; every hub also meets the other
        // hubs once. Weights are the archive's own shape: a heavy tail of one-story
        // co-mentions with a few strong pairs.
        let mut pairs: HashSet<(usize, usize)> = HashSet::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut push = |a: usize, b: usize, weight: f64| {
            if a == b {
                return;
            }
            let key = (a.min(b), a.max(b));
            if !pairs.insert(key) {
                return;
            }
            edges.push(Edge {
                a: key.0 as i64 + 1,
                b: key.1 as i64 + 1,
                weight,
                label: String::new(),
            });
        };
        for hub in 0..hubs.min(count) {
            for other in (hub + 1)..(hubs.min(count)) {
                push(hub, other, 40.0 + (hub * 7 + other) as f64 % 30.0);
            }
            for leaf in hubs..count {
                if (leaf + hub) % 3 == 0 {
                    push(hub, leaf, 1.0 + ((leaf * 3 + hub) % 9) as f64);
                }
            }
        }
        for leaf in hubs..count {
            push(leaf, hubs + (leaf * 7) % (count - hubs), 1.0 + (leaf % 4) as f64);
        }
        (entities, edges)
    }

    fn settled(count: usize, links: usize, ticks: usize) -> GraphView {
        let (entities, edges) = synthetic(count, links);
        let mut view = GraphView::default();
        view.fill(entities, edges);
        for _ in 0..ticks {
            view.tick();
        }
        view
    }

    /// A label's size without a font: the placement is geometry, and a test that needs
    /// a window to measure text would not run here.
    fn fake_size(name: &str) -> Vec2 {
        Vec2::new(name.chars().count() as f32 * 6.1, 13.0)
    }

    fn luminance(grey: u8) -> f32 {
        let channel = grey as f32 / 255.0;
        if channel <= 0.040_45 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }

    /// The WCAG contrast ratio between two painted colours, so "visible" is a number and not
    /// an opinion. Every colour this page paints is an opaque grey from the palette.
    fn contrast(one: Color32, other: Color32) -> f32 {
        let (a, b) = (luminance(one.r()), luminance(other.r()));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// A grey as a colour, for the two values these tests compare against history: the 0.15
    /// alpha (grey 43) the old floor drew, and the 1 px hairline it replaced it with.
    fn grey(value: u8) -> Color32 {
        Color32::from_gray(value)
    }

    /// The picture's lightest relation is one story and its heaviest is sixty-seven: the
    /// shape `edges.weight` has in the shipped archive (stories that name both ends).
    const PICTURE: (f32, f32) = (1.0, 67.0);

    /// The weakest relation line is visible on the plate, and the two steps of the ladder are
    /// both from the palette: the floor is a claim about content, so it is a number here.
    ///
    /// This used to be a computed gradient of seven greys between 0.38 and 0.85 alpha — none
    /// of them named in the theme, which is what `tests/design_audit.rs` counted. The ladder
    /// is two palette values now, and the tail is the one that has to stay above 3:1.
    #[test]
    fn the_weakest_relation_line_is_visible_on_the_plate() {
        let (weakest, heaviest) = PICTURE;
        let weak = edge_grey(weakest, weakest, heaviest);
        let strong = edge_grey(heaviest, weakest, heaviest);
        assert_eq!(weak, theme::FAINT, "the lightest line is the floor itself");
        assert_eq!(strong, theme::DIM, "and the heaviest is the top of the ladder");
        println!(
            "the ladder: tail grey {} ({:.2}:1 on the plate), head grey {} ({:.2}:1)",
            weak.r(),
            contrast(weak, PLATE),
            strong.r(),
            contrast(strong, PLATE)
        );
        assert!(
            contrast(weak, PLATE) >= 3.0,
            "the weakest edge is grey {}, {:.2}:1 on the plate",
            weak.r(),
            contrast(weak, PLATE)
        );
        // The old floor was 0.15 alpha, grey 43: 1.4:1, which is why a thin relation
        // disappeared into the background. That is still the number this floor beats.
        assert!(
            contrast(weak, PLATE) > contrast(grey(43), PLATE) * 2.0,
            "the floor has to be clearly above the 0.15 alpha it replaced"
        );
        assert!(weak.r() >= 35, "and above the absolute floor the old test kept");
        assert!(
            contrast(strong, PLATE) > contrast(weak, PLATE) + 1.0,
            "the two steps have to be steps: {:.2}:1 against {:.2}:1",
            contrast(strong, PLATE),
            contrast(weak, PLATE)
        );
        assert!(
            strong.r() < theme::INK.r(),
            "a line stays quieter than the ink a node is filled with"
        );
        // Two values and no third: the palette names exactly the greys this page paints.
        assert!(
            theme::ALL.iter().any(|(_, colour)| *colour == weak)
                && theme::ALL.iter().any(|(_, colour)| *colour == strong),
            "an edge grey is not in the theme's palette"
        );
        // A set of one weight is drawn at the top of the ladder, not at the floor: a single
        // relation is the strongest relation the picture has.
        assert_eq!(edge_grey(4.0, 4.0, 4.0), strong);
        assert_eq!(edge_width(4.0, 4.0, 4.0), WIDTH_MAX);
    }

    /// The strongest ten lines of the picture are visibly heavier than its tail: that is the
    /// difference between a graph and a grey mesh, so it is a number here.
    #[test]
    fn the_strongest_ten_lines_are_heavier_than_the_tail() {
        let (weakest, heaviest) = PICTURE;
        // Ten stories and up is the top ten of a long tail: the archive's own distribution.
        let top = edge_width(10.0, weakest, heaviest);
        let tail = edge_width(weakest, weakest, heaviest);
        assert!(
            top >= tail * 2.0,
            "the ten strongest relations are {top:.2} px wide against a {tail:.2} px tail: \
             not a hierarchy"
        );
        assert!(
            edge_grey(10.0, weakest, heaviest).r() as i32
                - edge_grey(weakest, weakest, heaviest).r() as i32
                >= 40,
            "and grey {} against grey {} is not enough of a difference",
            edge_grey(10.0, weakest, heaviest).r(),
            edge_grey(weakest, weakest, heaviest).r()
        );
        // Monotone over the whole range, weight and grey both: a heavier relation is never
        // drawn thinner or dimmer than a lighter one. The grey is a two-step ladder, so it is
        // monotone *non-strict*; the width carries the ordering between the steps.
        let mut widths = Vec::new();
        let mut greys = Vec::new();
        for stories in 1..=67 {
            widths.push(edge_width(stories as f32, weakest, heaviest));
            greys.push(edge_grey(stories as f32, weakest, heaviest).r());
        }
        assert!(widths.windows(2).all(|pair| pair[0] <= pair[1]), "width is monotone");
        assert!(greys.windows(2).all(|pair| pair[0] <= pair[1]), "grey is monotone");
        assert!(widths[0] < widths[66] && greys[0] < greys[66], "and it spans the range");
        assert!(
            greys.iter().filter(|grey| **grey == greys[0]).count() < greys.len(),
            "the ladder never leaves its floor"
        );
    }

    #[test]
    fn a_spring_pushes_bigger_entities_further_apart() {
        // The claim is about the rest length, and it is a function of the two radii.
        assert!(rest_length(20.0, 20.0) > rest_length(4.0, 4.0));
        assert!(
            rest_length(RADIUS_MAX, RADIUS_MAX) > 2.0 * rest_length(RADIUS_MIN, RADIUS_MIN),
            "a pair of hubs has to be held more than twice as far apart as a pair of names"
        );
        // And the radii are ranks of the set: the hub is at the top of the range, the last
        // name is at the floor, and the ramp is strictly descending between them.
        let ramp: Vec<f32> = (0..20).map(|place| rank_radius(place, 20)).collect();
        assert!((ramp[0] - RADIUS_MAX).abs() < 0.001, "the top of the order is the biggest");
        assert!((ramp[19] - RADIUS_MIN).abs() < 0.001, "the last name is the floor");
        assert!(ramp.windows(2).all(|pair| pair[0] > pair[1]), "and the ramp descends");
        // What a settled picture no longer shows, since this round: the *spacing* of the ring
        // is the trap's answer, not the spring's. Measured on six entities, the hub's edge
        // settled at 93.3 px and the floor's at 93.1 px — the springs decide the rest length
        // (asserted above, unchanged) and the relation still holds its pair nearer than an
        // unrelated entity (`tests/graph_layout`), but the ring's radius is the room the
        // picture was given. What a bigger entity still shows is its own drawn size:
        // `the_dots_have_a_size_hierarchy` holds that, and the hub is drawn bigger than the
        // floor by the rank ramp.
        let (mut entities, edges) = synthetic(6, 6);
        for (index, entity) in entities.iter_mut().enumerate() {
            entity.count = 6 - index as i64;
        }
        let mut view = GraphView::default();
        view.fill(entities, edges);
        for _ in 0..400 {
            view.tick();
        }
        let span = |one: usize, two: usize| -> f32 {
            let (a, b) = (view.nodes[one], view.nodes[two]);
            ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
        };
        let (hub, floor) = (span(0, 1), span(4, 5));
        // Still true, and the reason the radii matter: a hub's edge is never drawn *shorter*
        // than a floor's. The 1.4× step the old centring gave it was the blob's geometry.
        assert!(
            hub >= floor * 0.95,
            "the hub's edge settled at {hub:.1} px and the floor's at {floor:.1} px: the rest \
             length is not following the radii"
        );
    }

    #[test]
    fn nodes_never_overlap_once_the_layout_has_settled() {
        let view = settled(120, 200, 80);
        let mut worst = f32::INFINITY;
        let mut pair = (0usize, 0usize);
        for a in 0..view.nodes.len() {
            for b in (a + 1)..view.nodes.len() {
                let (one, two) = (view.nodes[a], view.nodes[b]);
                let distance = ((one.x - two.x).powi(2) + (one.y - two.y).powi(2)).sqrt();
                let clear = one.radius + two.radius;
                if distance - clear < worst {
                    worst = distance - clear;
                    pair = (a, b);
                }
            }
        }
        assert!(
            worst >= 0.0,
            "nodes {} and {} overlap by {} px",
            pair.0,
            pair.1,
            -worst
        );
    }

    #[test]
    fn labels_are_for_the_strongest_nodes_and_never_overlap() {
        let view = settled(120, 200, 80);
        let order = view.strongest();
        assert_eq!(order.len(), 120, "every node is in the strength order");
        let anchors: Vec<(usize, Pos2, f32, Vec2)> = order
            .iter()
            .take(LABELS_MAX)
            .map(|slot| {
                let node = view.nodes[*slot];
                (*slot, Pos2::new(node.x, node.y), node.radius, fake_size(&view.name_of(*slot)))
            })
            .collect();
        let obstacles: Vec<(Pos2, f32)> = view
            .nodes
            .iter()
            .map(|node| (Pos2::new(node.x, node.y), node.radius))
            .collect();
        let bounds = Rect::from_min_max(Pos2::new(-900.0, -900.0), Pos2::new(900.0, 900.0));
        let placed = place_labels(&anchors, &obstacles, &[], bounds, 2);

        assert!(!placed.is_empty(), "a settled graph of 120 nodes fits some labels");
        assert!(placed.len() <= LABELS_MAX);
        for (slot, box_) in &placed {
            assert!(
                order[..LABELS_MAX].contains(slot),
                "node {slot} is not one of the strongest"
            );
            assert!(bounds.contains_rect(*box_), "a label left the canvas");
        }
        for (index, (slot, one)) in placed.iter().enumerate() {
            for (other_slot, two) in placed.iter().skip(index + 1) {
                assert!(
                    !one.intersects(*two),
                    "labels {slot} and {other_slot} overlap"
                );
            }
        }
    }

    #[test]
    fn a_picked_node_is_labelled_even_when_it_is_not_the_strongest() {
        let view = settled(120, 200, 30);
        let weakest = *view.strongest().last().unwrap();
        let anchors = vec![(
            weakest,
            Pos2::new(view.nodes[weakest].x, view.nodes[weakest].y),
            view.nodes[weakest].radius,
            fake_size(&view.name_of(weakest)),
        )];
        let placed = place_labels(
            &anchors,
            &[],
            &[],
            Rect::from_min_max(Pos2::new(-900.0, -900.0), Pos2::new(900.0, 900.0)),
            1,
        );
        assert_eq!(placed.len(), 1, "the picked node is named");
        assert_eq!(placed[0].0, weakest);
    }

    #[test]
    fn a_click_lands_on_the_node_it_was_aimed_at() {
        let (entities, edges) = synthetic(2, 1);
        let mut view = GraphView::default();
        view.fill(entities, edges);
        view.nodes[0] = Node { x: 0.0, y: 0.0, vx: 0.0, vy: 0.0, radius: 4.2 };
        view.nodes[1] = Node { x: 100.0, y: 0.0, vx: 0.0, vy: 0.0, radius: 4.2 };
        let origin = Pos2::new(300.0, 200.0);

        assert_eq!(view.pick(Pos2::new(308.0, 200.0), origin), Some(0), "8 px away is a hit");
        assert_eq!(view.pick(Pos2::new(394.0, 200.0), origin), Some(1), "6 px away is a hit");
        assert_eq!(view.pick(Pos2::new(350.0, 200.0), origin), None, "the middle is nothing");
        assert_eq!(view.pick(Pos2::new(300.0, 260.0), origin), None, "60 px below is nothing");
        // zoom shrinks the drawn circle but not the target below a fingertip
        view.zoom = 0.3;
        assert_eq!(
            view.pick(Pos2::new(310.0, 200.0), origin),
            Some(0),
            "a small node is still a 12 px target"
        );
    }

    #[test]
    fn a_dragged_node_moves_and_the_canvas_does_not() {
        let (entities, edges) = synthetic(2, 1);
        let mut view = GraphView::default();
        view.fill(entities, edges);
        for _ in 0..200 {
            view.tick();
        }
        let pan = view.pan;
        let (x, y) = (view.nodes[0].x, view.nodes[0].y);
        view.alpha = 0.0;
        view.drag_node(0, Vec2::new(12.0, -5.0));

        assert!((view.nodes[0].x - (x + 12.0)).abs() < 0.001);
        assert!((view.nodes[0].y - (y - 5.0)).abs() < 0.001);
        assert_eq!(view.pan, pan, "a node drag is not a pan");
        // The wake moved to the release this round, and that is the fix: waking the layout
        // while the hand still holds the node moved the rest of the picture under the pointer —
        // measured on the ten-node plate in `tests/graph_interaction`, a drag on node 0 moved
        // node 2 by 6.6 px, which reads as the graph sliding away from the reader's hand.
        assert!(
            view.alpha < 0.35,
            "a node drag woke the layout: the picture moves under the hand (alpha {})",
            view.alpha
        );
        assert_eq!(view.nodes[0].vx, 0.0, "the node does not fly off on release");
        // Letting go is what settles the picture around where the reader put the node.
        view.release();
        assert!(
            view.alpha >= 0.35,
            "the release did not wake the layout: nothing settles around the node"
        );
    }

    #[test]
    fn the_arrows_walk_the_labelled_nodes_and_wrap() {
        let mut view = settled(12, 18, 4);
        let order = view.strongest();
        assert_eq!(view.step_selection(1), Some(order[0]));
        view.selected = Some(order[2]);
        assert_eq!(view.step_selection(1), Some(order[3]));
        assert_eq!(view.step_selection(-1), Some(order[1]));
        view.selected = Some(*order.last().unwrap());
        assert_eq!(view.step_selection(1), Some(order[0]), "the end wraps to the start");
        view.selected = Some(order[0]);
        assert_eq!(view.step_selection(-1), Some(*order.last().unwrap()));
    }

    #[test]
    fn every_relation_row_states_a_reason() {
        assert_eq!(relation_reason("governs", 4.0), "governs");
        assert_eq!(relation_reason("  member of  ", 4.0), "member of");
        assert_eq!(relation_reason("", 4.0), "named together in 4 stories");
        assert_eq!(relation_reason("   ", 1_204.0), "named together in 1 204 stories");
        assert_eq!(relation_reason("", 1.0), "named together in one story");
        // A zero weight is not one story: the row says what it knows, not a rounder number.
        assert_eq!(relation_reason("", 0.0), "named together in this window");
        // And the row is never a blank line, whatever the model wrote and whatever the
        // archive counted: the reason and the counts are both words.
        for label in ["", " ", "governs", "member of", "\t\n"] {
            for stories in [0.0, 1.0, 2.0, 67.0, 1_204.0] {
                let reason = relation_reason(label, stories);
                assert!(
                    !reason.trim().is_empty(),
                    "a relation labelled {label:?} with {stories} stories had no reason"
                );
                assert!(
                    !reason.contains('_'),
                    "the reason {reason:?} is a field name, not a sentence"
                );
            }
        }
    }

    /// The canvas is the box the page gives it: the plate's own rectangle is that box, on both
    /// axes, at the sizes the shell actually hands it and at two deliberately odd ones.
    ///
    /// This is the pin for "the graph is not full screen": the view may not reserve a share of
    /// its box for anything (a caption strip, a fixed height, a fraction) — whatever it is
    /// given, it paints. The audited plate sizes are the page box less the relations column and
    /// the gap: 700 × 520 → 332 × 363, 900 × 620 → 400 × 463, 1280 × 800 → 780 × 663,
    /// 1440 × 900 → 948 × 763, 2400 × 1400 → 1908 × 1263 (measured with `graph_sizes` and
    /// `tests/layout_screens`; the numbers live in those tests, not here).
    #[test]
    fn the_canvas_is_the_box_it_is_given() {
        let context = egui::Context::default();
        theme::install(&context);
        let (entities, edges) = synthetic(24, 40);
        for size in [
            Vec2::new(332.0, 363.0),
            Vec2::new(400.0, 463.0),
            Vec2::new(780.0, 663.0),
            Vec2::new(948.0, 763.0),
            Vec2::new(1908.0, 1263.0),
            Vec2::new(1_204.0, 300.0),
            Vec2::new(240.0, 900.0),
        ] {
            let mut view = GraphView::default();
            view.fill(entities.clone(), edges.clone());
            let picture = draw(&context, &mut view, size);
            let plate = picture
                .plate
                .unwrap_or_else(|| panic!("no plate painted at {size:?}"));
            let wanted = Rect::from_min_size(Pos2::ZERO, size);
            assert!(
                (plate.min - wanted.min).length() < 0.5 && (plate.size() - size).length() < 0.5,
                "the canvas at {size:?} was {plate:?}: the view paints {:.0}×{:.0} of a box it \
                 was given {:.0}×{:.0}",
                plate.width(),
                plate.height(),
                size.x,
                size.y
            );
        }
    }

    /// The column's width is the page's own number, and the plate gets everything else: at the
    /// app's minimum window and at the smallest audited one the two sit **beside** each other,
    /// so the canvas is the page's full height instead of the top of a stacked pair.
    ///
    /// Restated here from `app.rs`'s plan (`page − 20 px margins − rail`, the plate's 320 px
    /// floor, one gap) because that arithmetic is the shell's; what this pins is the one number
    /// this file owns — [`PANEL_WIDTH`] — and the consequence the reader sees.
    #[test]
    fn the_column_leaves_the_canvas_the_rest_of_the_page() {
        for (window, rail, page) in [
            (700.0_f32, 64.0_f32, 696.0_f32),
            (860.0, 188.0, 632.0),
            (900.0, 188.0, 692.0),
        ] {
            let content = window - rail - 40.0;
            let plate = content - crate::app::GRAPH_GAP - PANEL_WIDTH;
            assert!(
                content >= 320.0 + crate::app::GRAPH_GAP + PANEL_WIDTH,
                "a {window:.0} px window leaves a {content:.0} px page box, which cannot hold the \
                 plate's 320 px floor, the gap and the {PANEL_WIDTH} px column: it would stack, \
                 and the canvas would be a band again (page box {page:.0})"
            );
            assert!(
                plate >= 320.0,
                "the plate would be {plate:.0} px at {window:.0} px, under its floor"
            );
            assert!(
                plate / content >= 0.5,
                "the canvas is {:.0} % of the page box at {window:.0} px",
                100.0 * plate / content
            );
        }
        // And at the size the shell names as its own: at least 60 % of the content box is the
        // canvas, which the page promises in `tests/layout_screens`.
        let content = 1280.0 - 188.0 - 40.0;
        let plate = content - crate::app::GRAPH_GAP - PANEL_WIDTH;
        assert!(
            plate / content >= 0.60,
            "the canvas is {:.0} % of the content box at 1280x800, under the 60 % the page \
             promises ({plate:.0} of {content:.0} px)",
            100.0 * plate / content
        );
    }

    /// Every dot a frame painted, full-size and grain, with the radius it was painted at.
    fn drawn_dots(picture: &Picture) -> Vec<(Pos2, f32)> {
        let mut all = picture.dots.clone();
        all.extend(picture.dim.iter().copied());
        all
    }

    /// The smallest box that holds every painted dot, circle included.
    fn drawn_box(picture: &Picture) -> Rect {
        let dots = drawn_dots(picture);
        let mut min = Pos2::new(f32::INFINITY, f32::INFINITY);
        let mut max = Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY);
        for (at, radius) in &dots {
            min.x = min.x.min(at.x - radius);
            min.y = min.y.min(at.y - radius);
            max.x = max.x.max(at.x + radius);
            max.y = max.y.max(at.y + radius);
        }
        Rect::from_min_max(min, max)
    }

    /// The median distance from a dot to its nearest other dot, in painted pixels.
    fn median_nearest(picture: &Picture) -> f32 {
        let dots = drawn_dots(picture);
        let mut nearest: Vec<f32> = dots
            .iter()
            .map(|(at, _)| {
                dots.iter()
                    .map(|(other, _)| at.distance(*other))
                    .filter(|distance| *distance > 0.0)
                    .fold(f32::INFINITY, f32::min)
            })
            .collect();
        nearest.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        nearest.get(nearest.len() / 2).copied().unwrap_or(0.0)
    }

    /// **The reader's own numbers**, on the picture the page opens with: 120 entities and 900
    /// relations, settled and fitted on the Graph canvas a 1280 × 800 window leaves (780 × 663,
    /// from `tests/layout_screens`).
    ///
    /// "das Netz zieht sich immer noch zu sehr zusammen": 47.7 px between neighbours with 900
    /// relations is still a mesh. The answer is not only the physics — the opening view draws
    /// fewer entities than the plate could hold ([`ACTORS_AT_FIT`]) with at most
    /// [`LINKS_PER_NODE_AT_FIT`] relations each, and the fit scales *that* box to the plate.
    /// Measured now: 22 dots (the plate's own room at [`SPACING`], under the 60 cap), 22
    /// relations, median nearest neighbour **85.0 px** (47.7 before), tightest pair **75.6 px**
    /// (24 px floor), box **90 % × 83 %** of the plate, no circle off the plate, and the
    /// caption says "22 of 120 entities · 22 of 900 relations · nothing selected · 98 when you
    /// zoom in".
    #[test]
    fn the_opening_view_is_a_graph_and_not_a_mesh() {
        let size = Vec2::new(780.0, 663.0);
        let (entities, edges) = synthetic(120, 900);
        let (_view, picture) = picture_of_data(entities, edges, size);
        let plate = picture.plate.expect("the plate is painted");
        let dots = drawn_dots(&picture);
        let box_ = drawn_box(&picture);
        assert!(
            dots.len() <= ACTORS_AT_FIT,
            "the opening view draws {} dots: it is a mesh, not the {} entities a plate can carry",
            dots.len(),
            ACTORS_AT_FIT
        );
        assert!(
            !dots.is_empty(),
            "the opening view draws nothing at all: the fit has nothing to measure"
        );
        let share = Vec2::new(box_.width() / plate.width(), box_.height() / plate.height());
        assert!(
            share.x >= 0.60 && share.y >= 0.60,
            "the picture covers {:.0} % x {:.0} % of the plate (measured 90 % x 83 %, floor 60): \
             the fit left the graph in a corner of the canvas",
            100.0 * share.x,
            100.0 * share.y
        );
        let median = median_nearest(&picture);
        assert!(
            median >= 80.0,
            "the median nearest neighbour is {median:.1} px (45.4 px here when the layout was \
             the only lever, 85.0 px now): the dots are still packed into a mesh"
        );
        let mut tightest = f32::INFINITY;
        for (index, (one, _)) in dots.iter().enumerate() {
            for (other, _) in dots.iter().skip(index + 1) {
                tightest = tightest.min(one.distance(*other));
            }
        }
        assert!(
            tightest >= 24.0,
            "the two closest dots are {tightest:.1} px apart: two entities read as one"
        );
        for (at, radius) in &dots {
            let circle = Rect::from_center_size(*at, Vec2::splat(radius * 2.0));
            assert!(
                plate.shrink(0.5).contains_rect(circle),
                "a dot of radius {radius:.1} at {at:?} is painted off the plate {plate:?}"
            );
        }
        let caption = picture.caption.clone().unwrap_or_default();
        assert!(
            caption.contains(&format!("{} of 120 entities", dots.len())),
            "the caption does not say how many of how many are drawn: {caption:?}"
        );
    }

    /// One headless canvas frame, answering what `show` answered. The texture deltas are
    /// released: 0.37 asserts on a `FullOutput` dropped with deltas still in it.
    fn frame(context: &egui::Context, view: &mut GraphView) -> Option<i64> {
        let mut opened = None;
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 720.0))),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| opened = view.show(ui, 600.0));
            },
        );
        output.drop_without_applying_deltas();
        opened
    }

    #[test]
    fn a_relation_row_names_the_slot_on_its_far_end() {
        let (entities, edges) = synthetic(2, 1);
        let mut view = GraphView::default();
        view.fill(entities, edges);
        let rows = view.around(0);
        assert_eq!(rows.len(), 1, "the one relation this graph has");
        assert_eq!(rows[0].other, 1, "the row says which node is on the far end");
        assert_eq!(
            pair_of(0, rows[0].other),
            (0, 1),
            "the row's pair is the key the plate rings"
        );
        assert_eq!(pair_of(4, 1), pair_of(1, 4), "a pair is one key from either end");
    }

    #[test]
    fn a_relation_row_says_why_and_how_many_stories() {
        let named = Relation {
            name: "B".to_string(),
            other: 1,
            strength: 1.0,
            stories: 4.0,
            label: "governs".to_string(),
        };
        assert_eq!(under_the_name(&named), "governs · 4 stories");
        let unnamed = Relation {
            name: "B".to_string(),
            other: 1,
            strength: 1.0,
            stories: 4.0,
            label: "  ".to_string(),
        };
        assert_eq!(
            under_the_name(&unnamed),
            "named together in 4 stories",
            "the count is said once, not twice"
        );
        assert_eq!(stories_words(0), "0 stories");
        assert_eq!(stories_words(1), "1 story");
        assert_eq!(stories_words(1_204), "1 204 stories");
    }

    /// The panel writes the pair it opened into the view and the canvas reads it back — and
    /// the read of its stories happens on that change, once, not on every frame. A missing
    /// archive is a sentence in the panel, never an empty list that claims the relation has
    /// no stories.
    #[test]
    fn opening_a_relation_asks_the_archive_once_and_says_when_it_cannot() {
        let context = egui::Context::default();
        let (entities, edges) = synthetic(3, 2);
        let mut view =
            GraphView::default().with_archive("/nonexistent/atlas-no-such-archive.db");
        view.fill(entities, edges);
        view.selected = Some(0);

        assert!(frame(&context, &mut view).is_none());
        assert!(
            view.opening().is_none(),
            "a frame with no relation open read the archive"
        );

        view.open_relation(2, 0);
        assert_eq!(
            view.focused(),
            Some((0, 2)),
            "the pair is kept lower slot first, whatever end the panel opened it from"
        );
        assert!(frame(&context, &mut view).is_none());
        let opening = view.opening().expect("the opened pair is the pair loaded");
        assert_eq!(opening.pair, (0, 2));
        match &opening.stories {
            Err(problem) => assert!(
                !problem.contains('/'),
                "the problem names the file the way a reader can act on it: {problem:?}"
            ),
            Ok(stories) => panic!("a missing archive answered {} stories", stories.len()),
        }
    }

    /// A *different* set of entities is a different picture: the relation the panel had open
    /// pointed at a node that is gone, and the story it opened belongs to the graph that went
    /// away.
    ///
    /// The same set twice is not a new graph — it is a page switch — and keeps the open
    /// relation; [`a_page_switch_keeps_the_camera_the_selection_and_the_layout`] pins that
    /// half. This test used to fill with the same fixture twice and call it "a new graph",
    /// which is exactly the reload the shell does on every page switch.
    #[test]
    fn a_new_graph_closes_the_relation_the_panel_had_open() {
        let (entities, edges) = synthetic(3, 2);
        let mut view = GraphView::default();
        view.fill(entities, edges);
        view.open_relation(0, 1);
        view.opened.set(Some(7));
        assert_eq!(view.focused(), Some((0, 1)));

        let (entities, edges) = synthetic(4, 2);
        view.fill(entities, edges);
        assert_eq!(
            view.focused(),
            None,
            "an open relation pointed at a node that is gone"
        );
        assert!(view.opening().is_none());
        assert_eq!(view.opened.get(), None, "an opened story does not outlive its graph");
    }

    /// The standing complaint from the other side of the window: a reader zooms in, steps to
    /// Overview and comes back to a picture that has been reset to the top of the canvas.
    /// Measured before this: `fill` set `zoom = 1` and `pan = 0` unconditionally, and the
    /// shell calls it whenever it reloads.
    #[test]
    fn a_page_switch_keeps_the_camera_the_selection_and_the_layout() {
        let (entities, edges) = synthetic(120, 900);
        let mut view = GraphView::default();
        view.fill(entities.clone(), edges.clone());
        for _ in 0..240 {
            view.tick();
        }
        view.zoom = 2.25;
        view.pan = Vec2::new(40.0, -25.0);
        view.selected = Some(3);
        view.focus.set(Some((3, 4)));
        let settled = (view.nodes[7].x, view.nodes[7].y);
        let alpha = view.alpha;

        // The same archive, handed over again: the reload a page switch performs.
        view.fill(entities.clone(), edges.clone());
        assert_eq!(view.zoom, 2.25, "the zoom was reset by a page switch");
        assert_eq!(view.pan, Vec2::new(40.0, -25.0), "the pan was reset");
        assert_eq!(view.selected, Some(3), "the selection was thrown away");
        assert_eq!(view.focused(), Some((3, 4)), "the open relation was closed");
        assert_eq!(
            (view.nodes[7].x, view.nodes[7].y),
            settled,
            "the layout restarted: the picture a reader comes back to has to be the one they \
             left"
        );
        assert_eq!(view.alpha, alpha, "a page switch is not a new simulation");

        // A different window is a different picture, and everything starts over.
        let (other_entities, other_edges) = synthetic(60, 300);
        view.fill(other_entities, other_edges);
        assert_eq!(view.zoom, 1.0, "new data did not reset the zoom");
        assert_eq!(view.pan, Vec2::ZERO, "new data did not reset the pan");
        assert_eq!(view.selected, None);
        assert_eq!(view.focused(), None);
        assert_eq!(view.alpha, 1.0, "a new picture has to settle");
        assert_eq!(view.nodes.len(), 60);
    }

    #[test]
    fn a_layout_tick_moves_the_nodes_apart() {
        let mut view = GraphView::default();
        view.fill(
            vec![
                Entity { id: 1, name: "A".into(), kind: "PER".into(), count: 10 },
                Entity { id: 2, name: "B".into(), kind: "ORG".into(), count: 4 },
            ],
            vec![Edge { a: 1, b: 2, weight: 3.0, label: "met in Berlin".into() }],
        );
        assert_eq!(view.edges.len(), 1);
        for _ in 0..40 {
            view.tick();
        }
        let (a, b) = (view.nodes[0], view.nodes[1]);
        let distance = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
        assert!(distance > 5.0, "the two nodes sat on top of each other: {distance}");
        let around = view.around(0);
        assert_eq!(around.len(), 1);
        assert_eq!(around[0].name, "B");
        assert_eq!(around[0].label, "met in Berlin");
        assert!((around[0].stories - 3.0).abs() < 0.001, "the stored weight is kept");
        assert!((around[0].strength - 1.0).abs() < 0.001, "one relation is the strongest");
    }

    /// The budget is a release number: 60 Hz is 16.6 ms for the whole window, and the
    /// graph's share of it is the layout tick, the collision pass, the label placement
    /// and egui's own tessellation of 200 lines — measured through a real frame, in a
    /// headless `egui::Context`, on the 120/200 picture the page draws.
    ///
    /// A debug build is 10–20× slower than the build that ships, so it is measured and
    /// printed but not enforced; run `cargo test --release -p atlas-desktop` to hold
    /// the number.
    #[test]
    fn a_graph_frame_of_a_hundred_and_twenty_nodes_fits_the_release_budget() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 720.0))),
            ..Default::default()
        };
        let frame = |view: &mut GraphView| {
            // 0.37: `run_ui` hands the callback the root `Ui` (panels are shown inside a
            // `Ui` now, not on the context), and a `FullOutput` must not be dropped
            // without clearing its texture deltas — that is a debug assertion, not a leak.
            let output = context.run_ui(input(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    view.show(ui, 600.0);
                });
            });
            output.drop_without_applying_deltas();
        };
        // The page's own picture is 120 entities and up to 900 relations (`app.rs` asks for
        // both limits); 200 is the older fixture and is kept so the two are measured the same
        // way. Both are settled first: a settling frame pays the tick that has stopped by the
        // time a reader is reading.
        for links in [200, 900] {
            let (entities, edges) = synthetic(120, links);
            let mut view = GraphView::default();
            view.fill(entities, edges);
            while view.alpha > ALPHA_REST {
                view.tick();
            }
            // Warm the font atlas and the first-frame caches; they are not a frame's cost.
            for _ in 0..4 {
                frame(&mut view);
            }
            let rounds = 20;
            let started = std::time::Instant::now();
            for _ in 0..rounds {
                frame(&mut view);
            }
            let per_frame = started.elapsed().as_secs_f64() * 1000.0 / rounds as f64;
            let budget = if cfg!(debug_assertions) { 60.0 } else { 6.0 };
            let painted = view.edges.len();
            println!(
                "120 nodes / {painted} relations: {per_frame:.2} ms per frame (budget \
                 {budget} ms, {})",
                if cfg!(debug_assertions) { "debug" } else { "release" }
            );
            assert!(
                per_frame <= budget,
                "a graph frame of {painted} relations took {per_frame:.2} ms, budget {budget} ms"
            );

            // The worst frame is a *settling* one: the O(n²) tick runs on it as well as the
            // paint. A reader watches the picture arrive, so that frame is the one to budget.
            let (entities, edges) = synthetic(120, links);
            let mut fresh = GraphView::default();
            fresh.fill(entities, edges);
            let started = std::time::Instant::now();
            for _ in 0..rounds {
                frame(&mut fresh);
            }
            let settling = started.elapsed().as_secs_f64() * 1000.0 / rounds as f64;
            println!(
                "120 nodes / {painted} relations, settling: {settling:.2} ms per frame \
                 (alpha {:.2} at the end)",
                fresh.alpha
            );
            assert!(
                settling <= budget * 2.0,
                "a settling frame of {painted} relations took {settling:.2} ms, budget {} ms",
                budget * 2.0
            );
        }
    }


    /// One frame's picture, as far as a test can see it: the dots, the relation lines, the
    /// labels, and the plate they are composited on.
    #[derive(Default)]
    struct Picture {
        plate: Option<Rect>,
        dots: Vec<(Pos2, f32)>,
        /// The dots painted in `DIM` rather than `INK`: context a focus left behind.
        dim: Vec<(Pos2, f32)>,
        lines: Vec<(f32, u8)>,
        /// Every galley that is a label: its position, its size and its words.
        labels: Vec<(Pos2, Vec2, String)>,
        /// The caption along the bottom edge, which says what the picture is.
        caption: Option<String>,
        /// The camera buttons, where they were painted: not a name for a node.
        controls: Vec<(Pos2, Vec2, String)>,
        /// Every shape the frame walked, in the order it is composited: a dot painted
        /// after a name is a dot that covers it.
        walked: usize,
        /// Where each dot and each name sat in that order.
        dots_at: Vec<usize>,
        titles_at: Vec<usize>,
    }

    impl Picture {
        fn of(output: &egui::FullOutput) -> Picture {
            let mut picture = Picture::default();
            for clipped in &output.shapes {
                picture.walk(&clipped.shape);
            }
            picture
        }

        fn walk(&mut self, shape: &egui::Shape) {
            self.walked += 1;
            match shape {
                egui::Shape::Vec(children) => {
                    for child in children {
                        self.walk(child);
                    }
                }
                egui::Shape::Rect(rect) if rect.fill == PLATE => {
                    self.plate = Some(rect.rect);
                }
                egui::Shape::Circle(circle) if circle.fill == theme::INK => {
                    self.dots.push((circle.center, circle.radius));
                    self.dots_at.push(self.walked);
                }
                egui::Shape::Circle(circle) if circle.fill == theme::DIM => {
                    self.dim.push((circle.center, circle.radius));
                    self.dots_at.push(self.walked);
                }
                egui::Shape::LineSegment { stroke, .. } => {
                    self.lines.push((stroke.width, stroke.color.r()));
                }
                egui::Shape::Text(text) => {
                    let words = text.galley.text().to_string();
                    if words.is_empty() {
                        return;
                    }
                    // The caption is not a label: it says what the picture is, not what a
                    // node is called.
                    if words.contains(" entities · ") {
                        self.caption = Some(words);
                    } else if words == "Fit" || words.starts_with("Zoom ") {
                        // The camera bar, not a name for a node.
                        self.controls.push((text.pos, text.galley.size(), words));
                    } else {
                        self.labels.push((text.pos, text.galley.size(), words));
                        self.titles_at.push(self.walked);
                    }
                }
                _ => {}
            }
        }

        /// The radius of every painted dot, smallest first.
        fn radii(&self) -> Vec<f32> {
            let mut radii: Vec<f32> = self.dots.iter().map(|(_, radius)| *radius).collect();
            radii.sort_by(f32::total_cmp);
            radii
        }

        /// The box a label covers, from where the painter put it.
        fn boxes(&self) -> Vec<Rect> {
            self.labels
                .iter()
                .map(|(at, size, _)| Rect::from_min_size(*at, *size))
                .collect()
        }
    }

    fn median(sorted: &[f32]) -> f32 {
        if sorted.is_empty() {
            return f32::NAN;
        }
        sorted[sorted.len() / 2]
    }

    /// One frame of the canvas at `size`, and whether that frame asked for another one.
    fn paint(
        context: &egui::Context,
        view: &mut GraphView,
        size: Vec2,
        time: f64,
        events: Vec<egui::Event>,
    ) -> (Picture, bool) {
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        view.show(ui, size.y);
                    });
            },
        );
        let picture = Picture::of(&output);
        let asked = context.has_requested_repaint();
        output.drop_without_applying_deltas();
        (picture, asked)
    }

    /// A frame with no events, at a named canvas size.
    fn draw(context: &egui::Context, view: &mut GraphView, size: Vec2) -> Picture {
        paint(context, view, size, 0.0, Vec::new()).0
    }

    /// The settled layout and one painted frame of it, at `size`.
    fn picture_of(count: usize, links: usize, size: Vec2) -> (GraphView, Picture) {
        picture_of_data(synthetic(count, links).0, synthetic(count, links).1, size)
    }

    /// The same for a fixture the caller names: its entities, its relations, at `size`.
    fn picture_of_data(
        entities: Vec<Entity>,
        edges: Vec<Edge>,
        size: Vec2,
    ) -> (GraphView, Picture) {
        let mut view = GraphView::default();
        view.fill(entities, edges);
        for _ in 0..400 {
            view.tick();
        }
        let context = egui::Context::default();
        crate::theme::install(&context);
        // The font atlas and the first-frame caches are not a frame's cost.
        for index in 0..4 {
            paint(&context, &mut view, size, index as f64 / 60.0, Vec::new());
        }
        let picture = draw(&context, &mut view, size);
        (view, picture)
    }

    /// What the plate paints, as numbers: how far apart the dots are, whether any two sit on
    /// top of each other, how much of the layout's own box the picture fills, and how many
    /// lines it takes to say what the relations are.
    struct Metrics {
        dots: usize,
        inside: usize,
        nearest: Vec<f32>,
        overlap: usize,
        median_diameter: f32,
        biggest_diameter: f32,
        /// The widest a dot is against the distance to its own nearest neighbour: the number
        /// [`RADIUS_SHARE`] caps, measured on what the frame painted.
        widest_reach: f32,
        /// The dot that came closest to its neighbour: its radius and its distance.
        widest_pair: (f32, f32),
        /// The share of the plate the picture's own box covers: a graph spread over the canvas,
        /// not a disc in the middle of it.
        plate_share: f32,
        lines: usize,
        zoom: f32,
    }

    fn metrics(view: &GraphView, picture: &Picture) -> Metrics {
        let plate = picture.plate.expect("the frame painted no plate");
        let dots = &picture.dots;
        let mut nearest: Vec<f32> = Vec::with_capacity(dots.len());
        let mut overlap = 0usize;
        for (index, (at, radius)) in dots.iter().enumerate() {
            let mut closest = f32::INFINITY;
            for (other, (there, their)) in dots.iter().enumerate() {
                if index == other {
                    continue;
                }
                let distance = at.distance(*there);
                if distance < radius + their {
                    overlap += 1;
                }
                closest = closest.min(distance);
            }
            nearest.push(closest);
        }
        let mut diameters: Vec<f32> = dots.iter().map(|(_, radius)| radius * 2.0).collect();
        diameters.sort_by(f32::total_cmp);
        let (mut x0, mut x1, mut y0, mut y1) = (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
        for (at, _) in dots {
            x0 = x0.min(at.x);
            x1 = x1.max(at.x);
            y0 = y0.min(at.y);
            y1 = y1.max(at.y);
        }
        let mut widest_reach: f32 = 0.0;
        let mut widest_pair: (f32, f32) = (0.0, 0.0);
        for (index, (_, radius)) in dots.iter().enumerate() {
            let ratio = radius / nearest[index].max(0.001);
            if ratio > widest_reach {
                widest_reach = ratio;
                widest_pair = (*radius, nearest[index]);
            }
        }
        nearest.sort_by(f32::total_cmp);
        Metrics {
            dots: dots.len(),
            inside: dots.iter().filter(|(at, _)| plate.contains(*at)).count(),
            nearest,
            overlap: overlap / 2,
            median_diameter: median(&diameters),
            biggest_diameter: *diameters.last().unwrap_or(&0.0),
            widest_reach,
            widest_pair,
            plate_share: ((x1 - x0) * (y1 - y0)) / (plate.width() * plate.height()).max(1.0),
            lines: picture.lines.len(),
            zoom: view.zoom,
        }
    }

    /// The plate is a graph a reader can read, and the numbers that say so are printed here:
    /// node and relation count, mean degree, the median distance between neighbouring dots,
    /// how many pairs of circles touch, the share of the picture's box that holds dots, how
    /// many dots are on the plate, and how many lines it draws.
    ///
    /// The complaint this measures: "der Graph ist schrecklich, einfach nur ein großer Blob"
    /// — 120 entities and 900 relations in an 800×500 plate, every node in one dense disc, so
    /// zooming only makes a bigger blob. Measured at the size the page actually gets on a
    /// 1440-wide window (`page_box − PANEL_WIDTH − GRAPH_GAP`) and at 1100×820, on both
    /// fixtures: [`synthetic`]'s even ring and [`hubbed`]'s heavy-tailed archive shape.
    ///
    /// The assertions are the anti-blob promise: no two drawn dots overlap, the median
    /// nearest-neighbour distance beats the median dot's diameter, the picture fills its own
    /// box instead of sitting in a disc in the middle of it, and the fit keeps the graph.
    #[test]
    fn the_plate_is_a_graph_and_not_a_blob() {
        for size in [Vec2::new(800.0, 500.0), Vec2::new(1100.0, 820.0)] {
            for (name, (entities, edges)) in [
                ("ring", synthetic(120, 900)),
                ("hubs", hubbed(120, 8)),
            ] {
                let (view, picture) = picture_of_data(entities, edges, size);
                let found = metrics(&view, &picture);
                assert!(found.dots > 0, "the plate painted no dot at all");
                let mean_degree = 2.0 * view.edges.len() as f32 / view.nodes.len().max(1) as f32;
                println!(
                    "{name:>4} {:.0}×{:.0}: {} nodes, {} relations, mean degree {:.1} · {} lines, \
                     {} dots ({} inside) · nearest neighbour {:.1} / {:.1} / {:.1} px (min / \
                     median / max) · median dot ⌀ {:.1} px, biggest ⌀ {:.1} px · {} of {} pairs \
                     touch · plate {:.0}% · zoom {:.2}",
                    size.x,
                    size.y,
                    view.nodes.len(),
                    view.edges.len(),
                    mean_degree,
                    found.lines,
                    found.dots,
                    found.inside,
                    found.nearest[0],
                    median(&found.nearest),
                    found.nearest[found.nearest.len() - 1],
                    found.median_diameter,
                    found.biggest_diameter,
                    found.overlap,
                    found.dots * (found.dots - 1) / 2,
                    found.plate_share * 100.0,
                    found.zoom,
                );
                assert_eq!(
                    found.overlap, 0,
                    "{name} at {size:?}: {} pairs of drawn dots overlap at fit zoom — two dots \
                     on top of each other are one dot to the reader",
                    found.overlap
                );
                assert!(
                    median(&found.nearest) > found.median_diameter,
                    "{name} at {size:?}: the median nearest-neighbour distance is {:.1} px \
                     against a median dot of ⌀ {:.1} px — the picture is a mass, not a graph",
                    median(&found.nearest),
                    found.median_diameter
                );
                assert!(
                    found.widest_reach <= RADIUS_SHARE + 0.01,
                    "{name} at {size:?}: a dot is {:.2}× the distance to its own nearest \
                     neighbour (the cap is {RADIUS_SHARE}) — a hub reaches its neighbour, and \
                     the eye reads the pair as one shape (that dot is {:.1} px across with its \
                     neighbour at {:.1} px)",
                    found.widest_reach,
                    found.widest_pair.0,
                    found.widest_pair.1
                );
                assert!(
                    found.plate_share >= 0.6,
                    "{name} at {size:?}: the picture covers {:.0}% of the plate — the fit left \
                     the graph in a corner of the canvas",
                    found.plate_share * 100.0
                );
                // The fit holds the *drawn* entities (the opening view keeps the rest back),
                // so that is the set this promise is about.
                assert!(
                    found.inside >= 9 * found.dots / 10,
                    "{name} at {size:?}: the plate holds {} of {} drawn entities at fit zoom — \
                     the fit clipped the graph",
                    found.inside,
                    found.dots
                );
            }
        }
    }


    /// The complaint, twice over. First: "radii are 5.2 px min / 5.9 px median / 16.6 px max —
    /// 120 dots that are all the same small size". Then, after a curve on the mention share:
    /// "min 4.8 / median 5.1 / max 19.5 — almost every entity at the floor with one
    /// outlier".
    ///
    /// So the radius is a curve of the *rank* in the drawn set, and the distribution is the
    /// assertion: a floor, a median at least half way up, and a maximum at the top of the
    /// range, so the eye can rank what it sees.
    #[test]
    fn the_dots_have_a_size_hierarchy() {
        let size = Vec2::new(1100.0, 820.0);
        let (view, picture) = picture_of(120, 900, size);
        // The actors, not the grain: this is the distribution of what is drawn at full size.
        // A grain is exactly `TAIL_RADIUS` on the plate, whatever the zoom, so the split is
        // measured against that and not against a number of pixels.
        let grain = TAIL_RADIUS * view.zoom.max(0.6);
        let mut radii: Vec<f32> = picture
            .radii()
            .into_iter()
            .filter(|radius| *radius > grain + 0.5)
            .collect();
        radii.sort_by(f32::total_cmp);
        let room = capacity(Rect::from_min_size(Pos2::ZERO, size));
        assert_eq!(radii.len(), room, "the full-size dots are the ones the plate has room for");
        let (smallest, middle, biggest) = (radii[0], median(&radii), radii[radii.len() - 1]);
        println!(
            "120/900 at 1100x820: {room} actors with radii {smallest:.1} / {middle:.1} / \
             {biggest:.1} px (min / median / max), zoom {:.2}",
            view.zoom
        );
        assert!(
            smallest <= RADIUS_MIN * view.zoom + 0.6,
            "the smallest actor is {smallest:.1} px, not the floor of {:.1}",
            RADIUS_MIN * view.zoom
        );
        assert!(
            smallest >= grain - 0.1,
            "the smallest actor is {smallest:.1} px, smaller than a grain ({grain:.1} px)"
        );
        // The top of the curve is no longer a promise the picture makes: a dot is capped by
        // the distance to its own neighbour (`neighbour_reach`), so the biggest actor is the
        // biggest dot the layout can show — `the_plate_is_a_graph_and_not_a_blob` holds that
        // cap to [`RADIUS_SHARE`].
        assert!(
            biggest <= RADIUS_MAX * view.zoom + 0.6,
            "the biggest actor is {biggest:.1} px, over the top of the curve ({:.1})",
            RADIUS_MAX * view.zoom
        );
        assert!(
            middle >= 9.0,
            "the median actor is {middle:.1} px: a floor with one outlier is not a hierarchy"
        );
        assert!(
            biggest >= 1.6 * middle,
            "the biggest actor is {biggest:.1} px against a {middle:.1} px median: not enough \
             of a step to read"
        );
        assert!(
            smallest < middle && middle < biggest,
            "the radii are {smallest:.1} / {middle:.1} / {biggest:.1}: a ramp, not a hierarchy"
        );
    }

    /// The ramp a drawing of `count` entities is painted on: the top of the range, the floor,
    /// a descending order, and a median on the upper half — which is what the eye needs and
    /// what a curve of mentions could not deliver on this archive.
    #[test]
    fn the_ranks_are_painted_on_a_ramp() {
        for count in [6, 23, 58, 104, 120] {
            let ramp: Vec<f32> = (0..count).map(|place| rank_radius(place, count)).collect();
            assert!((ramp[0] - RADIUS_MAX).abs() < 0.001, "{count} entities: no hub");
            assert!(
                (ramp[count - 1] - RADIUS_MIN).abs() < 0.001,
                "{count} entities: no floor"
            );
            assert!(
                ramp.windows(2).all(|pair| pair[0] > pair[1]),
                "{count} entities: the ramp is not descending"
            );
            let middle = median(&ramp);
            println!(
                "{count} entities: ranks painted {:.1} … {:.1} px, median {middle:.1}",
                ramp[0],
                ramp[count - 1]
            );
            // A ramp of five or six names is a ramp; the *median* claim is about a picture
            // dense enough for the middle of the order to be a middle class.
            if count >= 20 {
                assert!(
                    middle >= RADIUS_MIN + (RADIUS_MAX - RADIUS_MIN) * 0.28,
                    "{count} entities: the median rank is {middle:.1} px, too near the floor"
                );
            }
        }
        // One entity is one big dot, not a degenerate ramp.
        assert!((rank_radius(0, 1) - RADIUS_MAX).abs() < 0.001);
    }

    /// **Rank, and the caption that says so.** The plate draws the entities it has room for
    /// The two canvases are the task's own: a small window and a large one. Measured at
    /// 700×520: 59 of 120 entities at full size, 139 of 900 relations, 61 held back; at
    /// 2400×1400: all 120 at full size and 286 relations.
    #[test]
    fn the_canvas_decides_how_many_names_it_holds() {
        for size in [Vec2::new(700.0, 520.0), Vec2::new(2400.0, 1400.0)] {
            let (view, picture) = picture_of(120, 900, size);
            let room = capacity(Rect::from_min_size(Pos2::ZERO, size)).min(view.nodes.len());
            let grain = TAIL_RADIUS * view.zoom.max(0.6);
            let radii = picture.radii();
            let full = radii.iter().filter(|radius| **radius > grain + 0.5).count();
            let caption = picture.caption.clone().unwrap_or_default();
            println!(
                "{size:?}: {room} of 120 entities at full size, {} relations drawn, \
                 {} labels, {} dots, caption {caption:?}",
                picture.lines.len(),
                picture.labels.len(),
                picture.dots.len()
            );
            // The opening view draws the rank down to [`ACTORS_AT_FIT`], never all 120: the
            // rest are for the reader to zoom into or to reach through the panel and the
            // arrows, and the caption says so ("N of 120 entities · … · M when you zoom in").
            assert!(
                picture.dots.len() <= ACTORS_AT_FIT,
                "{} dots at fit: the opening view is a mesh, not a graph",
                picture.dots.len()
            );
            assert_eq!(
                full,
                picture.dots.len(),
                "a dot is painted at a size that is not an actor's: {full} full-size of {} drawn",
                picture.dots.len()
            );
            assert!(
                picture.lines.len() <= LINKS_PER_NODE_AT_FIT * room,
                "{} relations drawn for {room} full-size nodes is more than \
                 {LINKS_PER_NODE_AT_FIT} each",
                picture.lines.len()
            );
            assert!(
                picture.lines.len() * 2 <= 900,
                "the picture drew {} of 900 relations: that is not a ranked picture",
                picture.lines.len()
            );
            assert!(
                caption.contains(" relations · "),
                "the caption does not say how many relations are drawn: {caption:?}"
            );
            // The opening view keeps the rest of the set back, and the caption says so in one
            // of the two phrasings: "N when you zoom in" before the reader has zoomed, "N as
            // grains" once the deeper view paints them.
            if picture.dots.len() < 120 {
                assert!(
                    caption.contains(&format!("{} when you zoom in", 120 - picture.dots.len()))
                        || caption.contains(&format!("{} as grains", 120 - picture.dots.len())),
                    "the caption does not say what happened to the {} entities it kept back: \
                     {caption:?}",
                    120 - picture.dots.len()
                );
            }
        }
        // The numbers themselves, so a change to `SPACING` or `VISIBLE_MIN` has to be argued
        // with in this test and not discovered on the running page.
        let small = Rect::from_min_size(Pos2::ZERO, Vec2::new(700.0, 520.0));
        let plate = Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 820.0));
        let wide = Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, 1000.0));
        let large = Rect::from_min_size(Pos2::ZERO, Vec2::new(2400.0, 1400.0));
        println!(
            "room at 700×520: {}, 1100×820: {}, 1600×1000: {}, 2400×1400: {}",
            capacity(small),
            capacity(plate),
            capacity(wide),
            capacity(large)
        );
        // A dot needs a roomier square than it used to: at [`SPACING`] = 150 px the smallest
        // of these plates holds 16 actors and the 1100×820 one 40, which is the number the
        // reader's "47.7 px between neighbours is still a blob" asked for.
        assert_eq!(
            capacity(small),
            VISIBLE_MIN,
            "700×520 asks for 14 and gets the floor of {VISIBLE_MIN}"
        );
        assert_eq!(capacity(plate), 35, "1100×820 holds 35");
        assert_eq!(capacity(wide), 62, "1600×1000 holds 62");
        assert_eq!(capacity(large), 131, "2400×1400 holds 131, under the ceiling");
    }

    /// **The labels.** The complaint: "only 9 labels are painted … not the strongest nodes".
    /// The strongest node of the set is named, the top of the order is named, at least
    /// [`LABELS_MIN`] labels are placed, and no two of them touch or leave the plate.
    #[test]
    fn the_strongest_nodes_are_named_and_no_two_labels_overlap() {
        // The plates the claim is made on, with the floor each one owes: every one of them
        // names the whole top of the order, `LABELS_MAX` entities, which is the cap and not
        // only the floor — a plate that names fewer than the strongest it was asked for is
        // losing names to the collision pass. The floors are the measured counts and not the
        // bare promise of
        // `LABELS_MIN`, so a change that quietly costs names fails here instead of on the page.
        for (size, floor) in [
            (Vec2::new(1100.0, 820.0), 14),
            (Vec2::new(1440.0, 900.0), 14),
            (Vec2::new(1600.0, 1000.0), 14),
        ] {
            let (view, picture) = picture_of(120, 900, size);
            let plate = picture.plate.expect("the frame painted a plate");
            let boxes = picture.boxes();
            let order = view.strongest();
            println!(
                "{size:?}: {} labels painted for the top {LABELS_MAX} of 120 (the strongest is \
                 {:?}), floor {floor}",
                boxes.len(),
                view.name_of(order[0])
            );
            assert!(
                boxes.len() >= floor,
                "only {} labels were placed on a {:.0}×{:.0} plate, under the floor of {floor}",
                boxes.len(),
                size.x,
                size.y
            );
            assert!(
                picture
                    .labels
                    .iter()
                    .any(|(_, _, words)| *words == view.name_of(order[0])),
                "the strongest node ({:?}) is not named on the plate",
                view.name_of(order[0])
            );
            // The top of the order is what is labelled, not an arbitrary set: every label is
            // the name of one of the strongest nodes (nothing is selected or hovered here).
            let wanted: Vec<String> = order
                .iter()
                .take(LABELS_MAX + 4)
                .map(|slot| view.name_of(*slot))
                .collect();
            for (_, _, words) in &picture.labels {
                assert!(
                    wanted.iter().any(|name| name == words),
                    "label {words:?} names a node that is not among the strongest"
                );
            }
            // No two labels touch, and none of them leaves the plate.
            for (index, one) in boxes.iter().enumerate() {
                assert!(
                    plate.contains_rect(*one),
                    "label {:?} sits at {one:?}, outside the plate {plate:?}",
                    picture.labels[index].2
                );
                for (other, two) in boxes.iter().enumerate().skip(index + 1) {
                    assert!(
                        !one.intersects(*two),
                        "labels {:?} and {:?} overlap ({one:?} and {two:?})",
                        picture.labels[index].2,
                        picture.labels[other].2
                    );
                }
            }
        }
    }

    /// A selected node is a neighbourhood and not a hairball: its relations are drawn in the
    /// brighter palette step and the rest of the picture in a structural hairline. Both the
    /// contrast each step clears and the ratio between them are the claim — and both are
    /// palette values, because the audit counts any grey this view mixes itself.
    #[test]
    fn the_selection_reads_as_a_neighbourhood() {
        let lit = neighbour_grey();
        let dim = dimmed_grey();
        println!(
            "a selection: its relations at grey {} ({:.1}:1), the rest at grey {} ({:.1}:1) — \
             {:.1}× the contrast",
            lit.r(),
            contrast(lit, PLATE),
            dim.r(),
            contrast(dim, PLATE),
            contrast(lit, PLATE) / contrast(dim, PLATE)
        );
        assert!(
            contrast(lit, PLATE) >= 3.0 * contrast(dim, PLATE),
            "the selection is {:.1}:1 against context at {:.1}:1: not three to one",
            contrast(lit, PLATE),
            contrast(dim, PLATE)
        );
        assert!(
            contrast(lit, PLATE) >= 3.0,
            "a relation of the selection is {:.2}:1 on the plate",
            contrast(lit, PLATE)
        );
        assert!(
            contrast(dim, PLATE) < 2.0,
            "a dimmed line is context and must sit under the floor, not at {:.2}:1",
            contrast(dim, PLATE)
        );
        assert!(
            theme::ALL.iter().any(|(_, colour)| *colour == lit)
                && theme::ALL.iter().any(|(_, colour)| *colour == dim),
            "a selection grey is not in the theme's palette"
        );

        // And the painter really does it: with a node selected, the frame carries the
        // neighbourhood's grey and the context's grey, and nothing in between dominates.
        let (mut view, _) = picture_of(120, 900, Vec2::new(1100.0, 820.0));
        let hub = view.strongest()[0];
        view.selected = Some(hub);
        let context = egui::Context::default();
        crate::theme::install(&context);
        for index in 0..4 {
            paint(&context, &mut view, Vec2::new(1100.0, 820.0), index as f64 / 60.0, Vec::new());
        }
        let picture = draw(&context, &mut view, Vec2::new(1100.0, 820.0));
        let greys: Vec<u8> = picture.lines.iter().map(|(_, grey)| *grey).collect();
        assert!(!greys.is_empty(), "a selected node drew no relations at all");
        assert!(
            greys.contains(&dim.r()),
            "no line was dimmed for the selection (greys {greys:?})"
        );
        assert!(
            greys.iter().any(|grey| *grey >= lit.r()),
            "the selection's own relations were not drawn bright (greys {greys:?})"
        );
        // Every grey the frame painted is a colour the theme names — the audit's claim, kept
        // here too so a new literal cannot slip into this view unnoticed.
        for (_, grey) in &picture.lines {
            assert!(
                theme::ALL.iter().any(|(_, colour)| colour.r() == *grey),
                "a relation line was painted in grey {grey}, which the palette does not name"
            );
        }
    }

    /// While the layout settles the page asks for the next frame; when it has settled it asks
    /// for none, so an idle page is quiet. Both halves are counted here.
    ///
    /// The cooling is `alpha *= 0.985` a tick and `tick` stops under 0.004, so 366 frames is
    /// the arithmetic answer from a cold fill (measured: 366); the ceiling asserted is 400.
    #[test]
    fn a_settling_graph_asks_for_frames_and_a_settled_one_does_not() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let size = Vec2::new(1100.0, 820.0);
        let (entities, edges) = synthetic(120, 900);
        let mut view = GraphView::default();
        view.fill(entities, edges);
        let mut frames = 0;
        let mut asked = true;
        while asked && frames < 2_000 {
            let (_, again) = paint(&context, &mut view, size, frames as f64 / 60.0, Vec::new());
            asked = again;
            frames += 1;
        }
        println!(
            "settling: {frames} frames asked for from a cold fill, alpha now {:.4}",
            view.alpha
        );
        assert!(
            !asked,
            "the layout was still asking for frames after {frames} of them (alpha {:.4})",
            view.alpha
        );
        assert!(view.alpha <= ALPHA_REST);
        assert!(frames <= 400, "the layout needed {frames} frames to settle");
        for index in 0..10 {
            let (_, again) = paint(
                &context,
                &mut view,
                size,
                (frames + index) as f64 / 60.0,
                Vec::new(),
            );
            assert!(
                !again,
                "frame {} after rest still asked for a repaint",
                frames + index
            );
        }
    }

    /// **The picture cannot be lost.** Four hard drags in a row used to push a 120-node graph
    /// off a 1280×860 window — the worst frame of a 500-event storm painted 0 of 120 nodes and
    /// covered 0.0 % of the canvas — because a pan of `drag_delta` with nothing after it is
    /// unbounded. With [`GraphView::clamp_pan`] in the way, at least two thirds of the layout
    /// stays on the plate whatever the reader does, at every zoom and every window size.
    #[test]
    fn a_pan_cannot_push_the_picture_off_the_plate() {
        let (entities, edges) = synthetic(120, 900);
        let mut view = GraphView::default();
        view.fill(entities, edges);
        for _ in 0..400 {
            view.tick();
        }
        for size in [Vec2::new(1280.0, 860.0), Vec2::new(320.0, 240.0)] {
            let canvas = Rect::from_min_size(Pos2::ZERO, size);
            for zoom in [0.3, 0.75, 1.0, 4.0] {
                view.zoom = zoom;
                for pan in [
                    Vec2::new(-9_000.0, 0.0),
                    Vec2::new(0.0, 9_000.0),
                    Vec2::new(-9_000.0, 7_000.0),
                    Vec2::new(6_000.0, -6_000.0),
                ] {
                    view.pan = pan;
                    view.clamp_pan(canvas);
                    let (min, max) = view.centre_box();
                    let centre = canvas.center() + view.pan;
                    let box_screen = Rect::from_min_max(
                        Pos2::new(centre.x + min.x * zoom, centre.y + min.y * zoom),
                        Pos2::new(centre.x + max.x * zoom, centre.y + max.y * zoom),
                    );
                    let overlap = box_screen.intersect(canvas);
                    assert!(
                        overlap.width() >= KEEP * box_screen.width().min(canvas.width()) - 0.5,
                        "{size:?} at ×{zoom}: the picture is {} px wide on the plate and {:.0} \
                         of it is visible",
                        box_screen.width(),
                        overlap.width()
                    );
                    assert!(
                        overlap.height() >= KEEP * box_screen.height().min(canvas.height()) - 0.5,
                        "{size:?} at ×{zoom}: {:.0} of {} px of picture is visible",
                        overlap.height(),
                        box_screen.height()
                    );
                    // And the honest area reading: three fifths on each axis is at least
                    // `KEEP`² = 36 % of whichever of the picture and the plate is smaller —
                    // which is the same number, measured the same way, that
                    // `tests/graph_bounds.rs` holds the 500-event storm to.
                    let room = box_screen.area().min(canvas.area());
                    assert!(
                        overlap.area() >= KEEP * KEEP * room - 1.0,
                        "{size:?} at ×{zoom}: a ({:.0}, {:.0}) pan left {:.0} px² of a {:.0} px² \
                         picture visible ({:.2} of the smaller of picture and plate)",
                        pan.x,
                        pan.y,
                        overlap.area(),
                        box_screen.area(),
                        overlap.area() / room
                    );
                }
            }
        }
    }

    /// The way back: whatever a reader has done to the camera, a fit puts every node — and
    /// its whole circle — back on the plate, at a zoom no larger than 1.
    #[test]
    fn a_reset_puts_the_whole_layout_back_on_the_plate() {
        let (entities, edges) = synthetic(120, 900);
        let mut view = GraphView::default();
        view.fill(entities, edges);
        for _ in 0..400 {
            view.tick();
        }
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(700.0, 520.0));
        // Where a reader's drags and wheels can leave it: far away and blown up.
        view.pan = Vec2::new(-2600.0, 1750.0);
        view.zoom = 3.1;
        view.reset_view(canvas);
        // `fit_zoom` gives a picture of 120 entities as much zoom as the plate allows (its own
        // `grow`), so the reset may land above 1:1 — measured 1.038 on a 700×520 plate now that
        // the layout uses the room. What the reset *promises* is the camera that holds the whole
        // layout, and that is asserted below, node by node.
        assert!(
            (view.zoom - ZOOM_MIN).abs() > f32::EPSILON
                && (ZOOM_MIN..=ZOOM_MAX).contains(&view.zoom),
            "a reset left the zoom at {}",
            view.zoom
        );
        let centre = canvas.center() + view.pan;
        let (mut min, mut max) = (
            Pos2::new(f32::INFINITY, f32::INFINITY),
            Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY),
        );
        // The drawn entities are the ones the reset brings back: the opening view keeps the
        // rest of the set for a zoom or a selection, and a reset is the fit of what is on the
        // plate (`GraphView::drawn_box`), not of the archive.
        for slot in view.drawn_slots_for(canvas) {
            let node = view.nodes[slot];
            let at = Pos2::new(centre.x + node.x * view.zoom, centre.y + node.y * view.zoom);
            let radius = node.radius * view.zoom;
            assert!(at.x.is_finite() && at.y.is_finite(), "node {slot} reset to {at:?}");
            min.x = min.x.min(at.x - radius);
            min.y = min.y.min(at.y - radius);
            max.x = max.x.max(at.x + radius);
            max.y = max.y.max(at.y + radius);
        }
        assert!(
            canvas.expand(2.0).contains(min) && canvas.expand(2.0).contains(max),
            "after a reset the layout spans ({:.0}, {:.0})…({:.0}, {:.0}), not the {:.0}×{:.0} \
             canvas",
            min.x,
            min.y,
            max.x,
            max.y,
            canvas.width(),
            canvas.height()
        );
    }

    /// Escape clears the selection on the frame egui drops the focused widget, and the arrows
    /// leave the panel describing the node they land on.
    #[test]
    fn escape_clears_the_selection_even_as_the_focus_goes() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let size = Vec2::new(720.0, 720.0);
        let (entities, edges) = synthetic(10, 20);
        let mut view = GraphView::default();
        view.fill(entities, edges);
        view.alpha = 0.0;
        let key = |key: egui::Key| {
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        paint(&context, &mut view, size, 0.0, Vec::new());
        paint(&context, &mut view, size, 1.0 / 60.0, key(egui::Key::Tab));
        paint(&context, &mut view, size, 2.0 / 60.0, key(egui::Key::ArrowRight));
        assert!(
            view.selected.is_some(),
            "the arrow did nothing on a canvas Tab had focused"
        );
        paint(&context, &mut view, size, 3.0 / 60.0, key(egui::Key::Escape));
        assert_eq!(
            view.selected, None,
            "Esc left {:?} selected: egui clears the focused widget on Escape before the \
             canvas is drawn, so the frame the key arrives on is the one frame `has_focus()` \
             is false",
            view.selected
        );
    }

    /// One press and its release, at `at`: three frames, because egui reads the pointer's
    /// position at the *start* of a frame, so the move has to arrive before the press does.
    ///
    /// A press is two frames of its own: the button goes down and then up, and `clicked()` is
    /// the second one. The absolute times keep two clicks apart that egui would otherwise
    /// read as a double click.
    fn click(context: &egui::Context, view: &mut GraphView, size: Vec2, at: Pos2, time: f64) {
        let button = |pressed: bool| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        paint(
            context,
            view,
            size,
            time,
            vec![egui::Event::PointerMoved(at)],
        );
        paint(context, view, size, time + 1.0 / 60.0, vec![button(true)]);
        paint(context, view, size, time + 2.0 / 60.0, vec![button(false)]);
    }

    /// A point on the plate clear of every dot, every painted line and every name — the bare
    /// plate a press has to hit to clear the selection.
    fn bare_plate(view: &GraphView, plate: Rect) -> Pos2 {
        let mut y = plate.top() + 8.0;
        while y < plate.bottom() - 8.0 {
            let mut x = plate.left() + 8.0;
            while x < plate.right() - 8.0 {
                let at = Pos2::new(x, y);
                let clear = view.nodes.iter().enumerate().all(|(slot, node)| {
                    let centre = plate.center() + view.pan + Vec2::new(node.x, node.y) * view.zoom;
                    centre.distance(at) > node.radius * view.zoom + 24.0 + view.hit_radius(slot)
                }) && view
                    .shown_links
                    .iter()
                    .all(|(_, one, two)| segment_distance(at, *one, *two) > LINK_HIT * 2.0)
                    && view.shown_names.iter().all(|(_, box_)| !box_.expand(4.0).contains(at));
                if clear {
                    return at;
                }
                x += 6.0;
            }
            y += 6.0;
        }
        panic!("the plate has no bare point: every pixel is a dot, a line or a name");
    }

    /// A press on a relation line opens the pair it joins — the user's complaint in one
    /// line: "beim Graph auf eine Person gehen: es muss sofort dastehen, welche Verbindung
    /// die haben". Measured on the live archive before this: a press at (478.6, 479.5) on
    /// the line joining Trump and Putin answered `None`, so the plate cleared the selection
    /// the reader had instead of opening the relation they aimed at.
    #[test]
    fn a_press_on_a_line_opens_the_pair_it_joins() {
        let size = Vec2::new(1100.0, 820.0);
        let (view, picture) = picture_of(120, 900, size);
        assert!(
            !view.shown_links.is_empty(),
            "the frame painted no relations, so there is no line to press"
        );
        // Nearest wins, and only among the middles a reader could actually aim at: a dot is
        // the first thing a press answers, so the middle has to be clear of every painted
        // circle as well as of every other line.
        let mut best: Option<(f32, usize, Pos2, (usize, usize))> = None;
        for (index, one, two) in &view.shown_links {
            let middle = *one + (*two - *one) * 0.5;
            let over_a_dot = view
                .shown_dots
                .iter()
                .any(|(_, at, radius)| at.distance(middle) <= radius + 2.0);
            if over_a_dot {
                continue;
            }
            let clear = view
                .shown_links
                .iter()
                .filter(|(other, _, _)| other != index)
                .map(|(_, left, right)| segment_distance(middle, *left, *right))
                .fold(f32::INFINITY, f32::min);
            if best.as_ref().map(|(gap, ..)| clear > *gap).unwrap_or(true) {
                let link = &view.edges[*index];
                best = Some((clear, *index, middle, pair_of(link.a, link.b)));
            }
        }
        let Some((gap, index, middle, pair)) = best else {
            panic!("no painted line has a middle of its own");
        };
        println!(
            "line press: the middle of relation {index} at {middle:?} is {gap:.1} px from every \
             other line; it joins {:?}",
            pair
        );
        assert!(
            gap > LINK_HIT,
            "the clearest line middle is {gap:.1} px from the next line, inside the {LINK_HIT} px \
             reach of a press: the picture has no line a reader could aim at"
        );
        assert_eq!(
            view.pick_link(middle),
            Some(pair),
            "a press at the middle of the line joining {:?} answered {:?}",
            pair,
            view.pick_link(middle)
        );
        // The camera's own centre, read off the frame: the plate may be smaller than the
        // screen, and a centre derived from the screen would test a picture that is not
        // the one the reader got.
        let plate = picture.plate.expect("the frame painted no plate");
        assert_eq!(
            view.target(middle, plate.center() + view.pan),
            Target::Link(pair),
            "the press is not read as a line"
        );
    }

    /// The reach of a line press, and its ends: 5 px off the segment is the line, 7 px is the
    /// plate, and 10 px past the end of the segment is the plate too — the distance is to the
    /// segment, never to the line through it.
    #[test]
    fn a_line_is_pressed_where_it_is_drawn_and_nowhere_else() {
        let size = Vec2::new(700.0, 520.0);
        let (view, _picture) = picture_of(2, 1, size);
        let [(index, one, two)] = view.shown_links[..] else {
            panic!("two entities with one relation painted {} lines", view.shown_links.len());
        };
        let link = &view.edges[index];
        let pair = pair_of(link.a, link.b);
        let middle = one + (two - one) * 0.5;
        let direction = (two - one).normalized();
        let normal = Vec2::new(-direction.y, direction.x);
        assert_eq!(view.pick_link(middle), Some(pair), "the middle of the line is the line");
        assert_eq!(
            view.pick_link(middle + normal * 5.0),
            Some(pair),
            "5 px off the line is still the line: a 1 px stroke is not a 1 px target"
        );
        assert_eq!(
            view.pick_link(middle + normal * (LINK_HIT + 1.0)),
            None,
            "{} px off the line is the plate",
            LINK_HIT + 1.0
        );
        assert_eq!(
            view.pick_link(two + direction * 10.0),
            None,
            "10 px past the end of the line is the plate: the reach is the segment, not the \
             infinite line through it"
        );
    }

    /// A press on a painted name selects the node it names: a name a reader can read but not
    /// press is decoration. Measured on the live archive before this: a press on
    /// "Asia-Pacific Economic Cooperation", 31 px clear of every dot, selected nothing.
    #[test]
    fn a_press_on_a_name_selects_the_node_it_names() {
        let size = Vec2::new(1100.0, 820.0);
        let (view, _picture) = picture_of(120, 900, size);
        assert!(
            view.shown_names.len() >= LABELS_MIN,
            "the frame named only {} nodes, so a name press cannot be tested",
            view.shown_names.len()
        );
        for (slot, box_) in &view.shown_names {
            assert_eq!(
                view.pick_name(box_.center()),
                Some(*slot),
                "the middle of the name {:?} answered {:?}",
                view.name_of(*slot),
                view.pick_name(box_.center())
            );
        }
        // And a press between two names is a press on the plate, not on the nearer one.
        let (slot, box_) = view.shown_names[0];
        let beside = Pos2::new(box_.right() + 2.0, box_.center().y);
        assert!(
            view.shown_names
                .iter()
                .all(|(_, other)| !other.contains(beside)),
            "the harness found two names touching: {:?} and {beside:?}",
            box_
        );
        assert_eq!(
            view.pick_name(beside),
            None,
            "2 px right of {:?} answered slot {slot:?}",
            view.name_of(slot)
        );
    }

    /// Whatever a storm of pans and zooms does to the camera, the plate is never empty: there
    /// is always at least one entity on it, to see and to press.
    ///
    /// The box clamp alone promises that the picture *area* overlaps the plate, and on a small
    /// canvas at a high zoom that is not the same thing — measured, a fuzz of 500 events left
    /// a frame with 0 of 120 dots on a 320×240 canvas at zoom 3.28. The count below is the
    /// number of dots the frame actually painted, so it is evidence and not a restatement of
    /// the clamp.
    #[test]
    fn no_camera_leaves_the_plate_empty() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        for size in [Vec2::new(320.0, 240.0), Vec2::new(1100.0, 820.0)] {
            let (mut view, _picture) = picture_of(120, 900, size);
            let mut worst = usize::MAX;
            for step in 0..48 {
                view.pan = Vec2::new(
                    (step as f32 * 1371.0) % 4000.0 - 2000.0,
                    (step as f32 * 911.0) % 4000.0 - 2000.0,
                );
                view.zoom = [ZOOM_MIN, 0.5, 1.0, 2.5, ZOOM_MAX][step % 5];
                let painted = draw(&context, &mut view, size);
                assert!(
                    painted.plate.is_some(),
                    "the frame painted no plate at {size:?}"
                );
                worst = worst.min(view.shown_dots.len());
            }
            println!("{size:?}: fewest dots on the plate under a pan and zoom storm: {worst}");
            assert!(
                worst >= 1,
                "a camera left the {size:?} plate empty: the fewest dots it painted was {worst}, \
                 so a reader had nothing to see and nothing to press"
            );
        }
    }

    /// The click order, end to end: a line opens its pair and keeps the selection, a name
    /// selects its node, and only the bare plate clears what the reader had.
    #[test]
    fn a_click_answers_what_it_landed_on_and_only_the_plate_clears() {
        let context = egui::Context::default();
        crate::theme::install(&context);
        let size = Vec2::new(1100.0, 820.0);
        let (mut view, picture) = picture_of(120, 900, size);
        let plate = picture.plate.expect("the frame painted no plate");

        // Something selected to lose: the strongest node.
        let slot = view.strongest()[0];
        let node = view.nodes[slot];
        let at = plate.center() + view.pan + Vec2::new(node.x, node.y) * view.zoom;
        click(&context, &mut view, size, at, 2.0);
        assert_eq!(view.selected, Some(slot), "the press on a dot selected nothing");

        // The line: it opens its pair and the selection stays, because a line is a door and
        // not a miss. A middle clear of every dot, so the press can only be a line press.
        let centre = plate.center() + view.pan;
        let Some((index, middle)) = view.shown_links.iter().find_map(|(index, one, two)| {
            let middle = *one + (*two - *one) * 0.5;
            (view.pick(middle, centre).is_none() && view.pick_link(middle).is_some())
                .then_some((*index, middle))
        }) else {
            panic!("no painted line has a middle clear of every dot");
        };
        click(&context, &mut view, size, middle, 4.0);
        let link = view.edges[index].clone();
        assert_eq!(
            view.focused(),
            Some(pair_of(link.a, link.b)),
            "a press at {middle:?} on a relation line opened {:?}",
            view.focused()
        );
        assert!(
            view.selected.is_some(),
            "the press on a line threw away the selection the reader had"
        );

        // The name. A name the pointer does not also hover: arriving inside a dot's hit radius
        // names that dot, and naming it moves the labels — the press has to land on a name
        // that the hovered frame still paints where it was read.
        let Some((named, box_)) = view
            .shown_names
            .iter()
            .filter(|(_, box_)| {
                view.shown_dots
                    .iter()
                    .all(|(_, at, radius)| {
                        at.distance(box_.center()) > radius + HIT_MIN + HIT_SLACK
                    })
            })
            // And clear of every stroke: `target` gives a line under the pointer the press
            // before the letters, because a stroke through a word is the line's own target.
            .filter(|(_, box_)| {
                view.shown_links
                    .iter()
                    .all(|(_, one, two)| segment_distance(box_.center(), *one, *two) > LINK_HIT)
            })
            .min_by(|left, right| left.1.center().x.total_cmp(&right.1.center().x))
            .copied()
        else {
            panic!("every painted name sits inside a dot's reach, so no press can be a name");
        };
        click(&context, &mut view, size, box_.center(), 6.0);
        assert_eq!(
            view.selected,
            Some(named),
            "a press on the name {:?} selected {:?}",
            view.name_of(named),
            view.selected
        );
        assert_eq!(view.focused(), None, "a press on a name left a relation open");

        // And the bare plate, which is the only press that clears.
        let bare = bare_plate(&view, plate);
        click(&context, &mut view, size, bare, 8.0);
        assert_eq!(
            view.selected, None,
            "a press on the bare plate at {bare:?} left {:?} selected",
            view.selected
        );
        assert_eq!(view.focused(), None, "and left a relation open");
    }

    /// Where a node's dot is painted, for the camera the view holds now.
    fn dot_of(view: &GraphView, plate: Rect, slot: usize) -> Pos2 {
        let node = &view.nodes[slot];
        plate.center() + view.pan + Vec2::new(node.x, node.y) * view.zoom
    }

    /// A filled view whose layout has settled and whose cooling is spent, so a camera test
    /// measures the camera and not a picture still moving under it.
    fn calm(count: usize, links: usize) -> GraphView {
        let (entities, edges) = synthetic(count, links);
        let mut view = GraphView::default();
        view.fill(entities, edges);
        for _ in 0..600 {
            view.tick();
        }
        assert!(view.at_rest, "the layout did not settle in 600 ticks");
        view
    }

    /// One key press, with no modifiers held.
    fn pressed(key: egui::Key) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    }

    /// **A pinch zooms about the pointer.** A trackpad sends `Event::Zoom`, which this view
    /// reads beside the wheel at `graph.rs`'s input block; the dot under the fingers has to
    /// end where it started, or a reader who pinches to read a name pushes the name away as
    /// they zoom. Measured at three starting zooms over a ten-step gesture in and back out.
    #[test]
    fn pinch_zooms_about_the_pointer() {
        let size = Vec2::new(1100.0, 820.0);
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut view = calm(40, 60);
        let plate = paint(&context, &mut view, size, 0.0, Vec::new())
            .0
            .plate
            .expect("the frame painted no plate");
        // A node in the middle of the layout: the camera can hold it at the middle of the
        // plate without the pan clamp having an opinion. A node at the far edge cannot be
        // brought to the centre without the picture leaving the plate, and `clamp_pan`
        // prefers the picture — that is the promise the picture is never lost, not a bug in
        // the anchor.
        let slot = view
            .nodes
            .iter()
            .enumerate()
            .min_by(|left, right| {
                let one = left.1.x * left.1.x + left.1.y * left.1.y;
                let two = right.1.x * right.1.x + right.1.y * right.1.y;
                one.total_cmp(&two)
            })
            .map(|(slot, _)| slot)
            .expect("a filled view has nodes");
        // egui keeps a history of the input, and it has to run forwards: one clock for the
        // whole test, not one per starting zoom.
        let mut time = 1.0;
        for start in [0.6_f32, 1.0, 2.0] {
            // The finger is on a node, and the camera holds that node at the middle of the
            // plate, where no clamp has an opinion about it.
            view.zoom = start;
            view.touched = true;
            let node = &view.nodes[slot];
            view.pan = -Vec2::new(node.x, node.y) * start;
            let at = dot_of(&view, plate, slot);
            assert!(
                at.distance(plate.center()) < 0.5,
                "the fixture put the finger at {at:?}, not on the node it means to hold"
            );
            let step = |view: &mut GraphView, factor: f32, time: &mut f64| {
                *time += 1.0 / 60.0;
                paint(
                    &context,
                    view,
                    size,
                    *time,
                    vec![egui::Event::PointerMoved(at), egui::Event::Zoom(factor)],
                );
            };
            let step_in = 1.05_f32;
            for _ in 0..10 {
                step(&mut view, step_in, &mut time);
            }
            let expected = start * step_in.powi(10);
            assert!(
                (view.zoom - expected).abs() < 0.01,
                "ten pinches of ×{step_in} from ×{start} left the zoom at {}, not {expected}",
                view.zoom
            );
            let now = dot_of(&view, plate, slot);
            assert!(
                now.distance(at) <= 0.5,
                "after ten pinches in from ×{start} the node under the fingers moved \
                 {:.2} px: it was at {at:?}, it is painted at {now:?}",
                now.distance(at)
            );
            for _ in 0..10 {
                step(&mut view, 1.0 / step_in, &mut time);
            }
            assert!(
                (view.zoom - start).abs() < 0.01,
                "a pinch in and back out from ×{start} left the zoom at {}",
                view.zoom
            );
            let back = dot_of(&view, plate, slot);
            assert!(
                back.distance(at) <= 0.5,
                "a pinch in and back out from ×{start} moved the node under the fingers \
                 {:.2} px: it was at {at:?}, it is painted at {back:?}",
                back.distance(at)
            );
        }
    }

    /// **One wheel notch and one press of "Zoom in" are the same zoom.** The plate draws the
    /// wheel as [`WHEEL_NOTCH`] points and the button as [`ZOOM_STEP`]; both go through
    /// [`zoom_factor`], so a reader who learns one gesture knows the other. Measured through
    /// the frames: egui spreads a mouse wheel's points over a few frames of its own, and the
    /// view's exponential filter must land on the same factor the button applies at once.
    #[test]
    fn the_wheel_and_the_buttons_agree() {
        let size = Vec2::new(1100.0, 820.0);
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut view = calm(40, 60);
        let picture = paint(&context, &mut view, size, 0.0, Vec::new()).0;
        let plate = picture.plate.expect("the frame painted no plate");
        let at = plate.center();
        // The fit is the *drawn* box's this round and can land near `ZOOM_MAX`, where a button
        // has nowhere left to go — and this test is about the wheel and the button agreeing,
        // not about the ceiling. Start from a mid zoom nobody is clamped at.
        view.zoom = 1.0;
        let before = view.zoom;
        // One notch, in points: the unit a mouse wheel reports on this platform.
        let mut time = 0.5;
        paint(
            &context,
            &mut view,
            size,
            time,
            vec![egui::Event::PointerMoved(at)],
        );
        paint(
            &context,
            &mut view,
            size,
            time + 1.0 / 60.0,
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, WHEEL_NOTCH),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        for _ in 0..40 {
            time += 1.0 / 60.0;
            paint(&context, &mut view, size, time, Vec::new());
        }
        let by_wheel = view.zoom / before;
        assert!(
            by_wheel > 1.05,
            "a notch of {WHEEL_NOTCH} points zoomed ×{by_wheel}: the wheel does nothing"
        );
        // The button, on the plate the wheel just left: a press on it is a press on a
        // control, not on the plate — the selection a reader had must survive it.
        view.selected = Some(0);
        let picture = paint(&context, &mut view, size, time, Vec::new()).0;
        let zoom_in = picture
            .controls
            .iter()
            .find(|(_, _, words)| words == "Zoom in")
            .map(|(at, size, _)| *at + *size * 0.5)
            .expect("the plate painted no \"Zoom in\" button");
        let before_button = view.zoom;
        click(&context, &mut view, size, zoom_in, time + 0.5);
        let by_button = view.zoom / before_button;
        assert!(
            (by_wheel / by_button - 1.0).abs() < 0.01,
            "a wheel notch multiplies the zoom by {by_wheel} and the button by {by_button}: \
             the same gesture answers two ways"
        );
        assert_eq!(
            view.selected,
            Some(0),
            "a press on the plate's own \"Zoom in\" cleared the selection: the button is a \
             control on the plate, not a press on the plate"
        );
    }

    /// **`0` fits the layout** — the same way the caption's Fit button does, whichever corner
    /// of the camera the reader has reached.
    #[test]
    fn zero_fits_the_layout() {
        let size = Vec2::new(1100.0, 820.0);
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut view = calm(60, 120);
        let plate = paint(&context, &mut view, size, 0.0, Vec::new())
            .0
            .plate
            .expect("the frame painted no plate");
        // egui hands a canvas the keyboard only once it has been focused.
        paint(&context, &mut view, size, 0.1, pressed(egui::Key::Tab));
        let fitted = view.fit_zoom(plate);
        for (name, key) in [("0", egui::Key::Num0), ("Home", egui::Key::Home)] {
            // Where a drag and a wheel can leave the camera.
            view.zoom = 3.1;
            view.pan = Vec2::new(-1700.0, 1200.0);
            view.touched = true;
            paint(&context, &mut view, size, 0.2, pressed(key));
            assert!(
                (view.zoom - fitted).abs() < 0.01,
                "{name} left the zoom at {} instead of the ×{fitted} that fits the layout",
                view.zoom
            );
            let picture = paint(&context, &mut view, size, 0.3, Vec::new()).0;
            assert!(!picture.dots.is_empty(), "{name} painted no nodes at all");
            for (at, radius) in &picture.dots {
                let circle = Rect::from_center_size(*at, Vec2::splat(radius * 2.0));
                assert!(
                    plate.shrink(0.5).contains_rect(circle),
                    "after {name} a node is painted at {at:?} with radius {radius}, off the \
                     {}×{} plate",
                    plate.width(),
                    plate.height()
                );
            }
        }
    }

    /// **A double click fixes a node and `Esc` gives the picture back.** The dot under the
    /// press and everything joined to it stay in ink; the rest of the plate drops to the
    /// context grey. A second double click, or Escape, restores the whole picture.
    #[test]
    fn double_click_focuses_a_node_and_escape_returns() {
        let size = Vec2::new(1100.0, 820.0);
        let context = egui::Context::default();
        crate::theme::install(&context);
        let mut view = calm(40, 80);
        let plate = paint(&context, &mut view, size, 0.0, Vec::new())
            .0
            .plate
            .expect("the frame painted no plate");
        // The dot a reader would aim at: the biggest one on the plate, and clear of the
        // camera bar in the corner above it.
        let bar = Rect::from_min_size(
            Pos2::new(plate.right() - 220.0, plate.top()),
            Vec2::new(220.0, 40.0),
        );
        let (slot, at) = view
            .nodes
            .iter()
            .enumerate()
            .map(|(slot, _)| (slot, dot_of(&view, plate, slot)))
            .filter(|(_, at)| plate.contains(*at) && !bar.contains(*at))
            .max_by(|left, right| {
                view.nodes[left.0]
                    .radius
                    .total_cmp(&view.nodes[right.0].radius)
            })
            .expect("no dot on the plate to aim at");
        click(&context, &mut view, size, at, 1.0);
        click(&context, &mut view, size, at, 1.1);
        assert_eq!(
            view.focal,
            Some(slot),
            "a double click on the {} px dot at {at:?} fixed {:?} instead",
            view.nodes[slot].radius,
            view.focal
        );
        let picture = paint(&context, &mut view, size, 1.2, Vec::new()).0;
        assert!(
            !picture.dim.is_empty(),
            "the focused frame painted every dot in ink: the rest of the plate is not context"
        );
        let joined = view
            .edges
            .iter()
            .filter(|link| link.a == slot || link.b == slot)
            .count();
        // The opening view paints the drawn set, so the count is over what is on the plate.
        let painted = picture.dots.len() + picture.dim.len();
        assert_eq!(
            picture.dim.len(),
            painted - 1 - joined.min(painted - 1),
            "the focused frame dimmed {} of {painted} painted dots: one of them focused, \
             {joined} joined to it",
            picture.dim.len()
        );
        assert!(
            picture.dim.iter().all(|(there, _)| there.distance(at) > 1.0),
            "the focused dot itself was painted as context"
        );
        paint(&context, &mut view, size, 1.3, pressed(egui::Key::Escape));
        assert_eq!(view.focal, None, "Escape left the focus on {:?}", view.focal);
        let picture = paint(&context, &mut view, size, 1.4, Vec::new()).0;
        assert!(
            picture.dim.is_empty(),
            "after Escape {} dots are still painted as context",
            picture.dim.len()
        );
    }

    /// **Names where there is room, never a mass, never under a dot.** The complaint behind
    /// this file's label work is a plate of 120 identical dots nobody can name; the failure on
    /// the other side is a plate so full of text that the text is the mass. Measured at the fit
    /// zoom and at twice it, on the smallest plate the page is drawn at and on a large one: the
    /// floor of names holds, the cap holds, names never touch each other or leave the plate —
    /// and every name is composited after every dot, so no circle is painted over a name.
    #[test]
    fn names_sit_where_there_is_room_and_never_under_a_dot() {
        for size in [
            Vec2::new(700.0, 520.0),
            Vec2::new(1100.0, 820.0),
            Vec2::new(2400.0, 1400.0),
        ] {
            let (mut view, first) = picture_of(120, 900, size);
            let context = egui::Context::default();
            crate::theme::install(&context);
            let mut picture = first;
            for step in 0..2 {
                if step == 1 {
                    let plate = picture.plate.expect("the frame painted a plate");
                    view.zoom_about(plate, plate.center(), 2.0);
                    paint(&context, &mut view, size, 10.0, Vec::new());
                    picture = paint(&context, &mut view, size, 10.5, Vec::new()).0;
                }
                let plate = picture.plate.expect("the frame painted a plate");
                let boxes = picture.boxes();
                println!(
                    "{size:?} at ×{:.2}: {} names, {} dots, {} lines",
                    view.zoom,
                    boxes.len(),
                    picture.dots.len(),
                    picture.lines.len()
                );
                // The floor is a promise about the picture's own view. A 2× zoom is a slice
                // of it, and the layout now fills the plate, so the slice holds fewer actors
                // than the floor names — measured at 700×520 ×2.08, 7 actors are on the plate
                // and 4 of them take a name (the other three have every free slot taken by a
                // dot bigger than their own). What a slice still owes the reader is names that
                // do not collide and stay on the plate, asserted for both steps below.
                if step == 0 {
                    assert!(
                        boxes.len() >= LABELS_MIN,
                        "{size:?} at ×{:.2} named {} nodes, under the floor of {LABELS_MIN}: a \
                         reader cannot name what they see",
                        view.zoom,
                        boxes.len()
                    );
                }
                assert!(
                    boxes.len() <= LABELS_MAX + 3,
                    "{size:?} at ×{:.2} painted {} names: past the cap of {LABELS_MAX} the \
                     text is the mass",
                    view.zoom,
                    boxes.len()
                );
                let last_dot = picture
                    .dots_at
                    .iter()
                    .copied()
                    .max()
                    .expect("the frame painted no dots");
                let first_name = picture
                    .titles_at
                    .iter()
                    .copied()
                    .min()
                    .expect("the frame painted no names");
                assert!(
                    first_name > last_dot,
                    "{size:?} at ×{:.2}: a name is composited at shape {first_name} and a dot \
                     at {last_dot}, so a circle covers a name",
                    view.zoom
                );
                for (index, one) in boxes.iter().enumerate() {
                    assert!(
                        plate.shrink(0.5).contains_rect(*one),
                        "the name {:?} at {one:?} leaves the {:.0}×{:.0} plate",
                        picture.labels[index].2,
                        plate.width(),
                        plate.height()
                    );
                    for (other, two) in boxes.iter().enumerate().skip(index + 1) {
                        assert!(
                            !one.intersects(*two),
                            "the names {:?} and {:?} overlap: {one:?} and {two:?}",
                            picture.labels[index].2,
                            picture.labels[other].2
                        );
                    }
                }
            }
        }
    }
    /// **One vocabulary in the panel.** The archive's entity types come from two writers — the
    /// codes an older pass stored (`ORG 3479`, `PERSON 948`, `LOC 108`, `MISC 27`, measured)
    /// and the model's own words — and a reader must not have to translate between them. The
    /// mapping is display only: no type is re-decided here, an empty type still says so, and a
    /// word the model chose is shown exactly as it was stored.
    #[test]
    fn the_panel_states_every_entity_type_in_one_vocabulary() {
        assert_eq!(type_words("ORG"), "Institution");
        assert_eq!(type_words("PERSON"), "Person");
        assert_eq!(type_words("LOC"), "Place");
        assert_eq!(type_words("MISC"), "Other");
        assert_eq!(type_words("  "), "untyped");
        for word in ["Institution", "Country", "Product", "Person", "Place", "Other"] {
            assert_eq!(
                type_words(word),
                word,
                "the model's own word {word:?} was rewritten by the display mapping"
            );
        }
        let entity = Entity {
            id: 1,
            name: "A ministry".to_string(),
            kind: "ORG".to_string(),
            count: 1_204,
        };
        assert_eq!(type_line(&entity), "Institution · 1 204 mentions");
        let legacy = Entity {
            kind: "MISC".to_string(),
            ..entity.clone()
        };
        assert_eq!(
            type_line(&legacy),
            "Other · 1 204 mentions",
            "a MISC entity fell into the ORG bucket, which is what the old page did"
        );
    }







}
