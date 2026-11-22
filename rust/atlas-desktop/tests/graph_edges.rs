//! Every visible thing on the Graph page is a target, and every target leads somewhere —
//! checked on the live archive, read-only.
//!
//! The page draws four kinds of thing: a node, a relation line, a name, and the panel's rows.
//! A reader can only use what answers a press, so each one is pressed here with a real
//! `PointerButton`, in a headless `egui::Context`, at the position the frame actually painted
//! it — never at a position this file computed from row heights or from a constant.
//!
//! Six claims:
//!
//! 1. **A node click selects an entity that exists** — the press lands on the circle under the
//!    pointer, the panel names that entity, and [`GraphView::around`] lists the relations the
//!    panel draws rows for.
//! 2. **Every relation row is a door with something behind it**: one press opens the pair
//!    (the panel fills the row and the plate rings both ends), the row states why the two are
//!    connected in words, and the stories listed under it name both ends.
//! 3. **A story row opens the article**: the id the panel answers is an article the archive
//!    holds, with the headline the row showed.
//! 4. **A click on a relation line opens that relation** — the pair the line joins, as
//!    [`GraphView::pick_link`] answers it and as the handler acts on it.
//! 5. **A click on a name selects the node it names** — [`GraphView::pick_name`] reads the
//!    letter boxes the painter was given, and the press selects that node.
//! 6. **Every painted line and every painted name is a target**, not a sample of them: the
//!    sweep presses all of them and asks where each press landed.
//!
//! Claims 4 and 5 were red until `fix-graph` added the two hit-tests; 6 is the measurement
//! that closes the question rather than sampling it.
//!
//! The archive is the checkout's own `data/atlas.db`, opened through
//! [`atlas_desktop::data::Archive`] and through [`stories_for`], both read-only. A checkout
//! without one prints why and returns: the fixture-level claims are `graph_links.rs` and
//! `graph_interaction.rs`.
//!
//! Run with `--nocapture` to read the value each claim resolved:
//! `cargo test --offline -p atlas-desktop --test graph_edges -- --nocapture`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use atlas_desktop::data::{Archive, Edge, Entity};
use atlas_desktop::theme;
use atlas_desktop::views::graph::{panel, stories_for, GraphView, PANEL_WIDTH, STORIES_SHOWN};
use egui::{Color32, Pos2, Rect, Vec2};

/// The plate's near-black and a node's grey, read back out of the frame (see `views::graph`).
const PLATE: Color32 = Color32::from_rgb(7, 7, 7);
/// The node ink: a dot is filled with the theme's own INK, not a grey of its own.
const NODE: Color32 = atlas_desktop::theme::INK;

// --------------------------------------------------------------------------- the archive

/// The checkout's archive: `rust/atlas-desktop` sits two directories below the root.
fn archive_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/atlas.db")
}

/// The live archive, or `None` on a checkout without one, with a line saying so.
fn live() -> Option<(Archive, PathBuf)> {
    let path = archive_path();
    if !path.is_file() {
        println!("skipped: no archive at {}", path.display());
        return None;
    }
    match Archive::open(&path) {
        Ok(archive) => Some((archive, path)),
        Err(problem) => {
            println!("skipped: {problem}");
            None
        }
    }
}

/// The graph the page fills: the same two reads `app::Atlas::load` makes, the same limits.
fn filled(archive: &Archive, path: &Path) -> (GraphView, Vec<Entity>, Vec<Edge>) {
    let entities = archive.entities(168, 120);
    let ids: Vec<i64> = entities.iter().map(|entity| entity.id).collect();
    let edges = archive.edges(&ids, 900);
    let mut view = GraphView::default().with_archive(path);
    view.fill(entities.clone(), edges.clone());
    // The layout is run to rest and then frozen, so a difference between two frames is a
    // difference a press made and not one the simulation made in the meantime.
    for _ in 0..240 {
        view.tick();
    }
    assert!(
        view.alpha <= 0.05,
        "the real layout never came to rest (alpha {}), so no click below can be aimed \
         at a position that stays put",
        view.alpha
    );
    view.alpha = 0.0;
    (view, entities, edges)
}

// --------------------------------------------------------------------------- reading a frame

/// What one frame painted, as far as a pointer can tell: the plate, the circles, the lines,
/// and every piece of text with where it starts and how big it is.
struct Painted {
    plate: Rect,
    nodes: Vec<(Pos2, f32)>,
    lines: Vec<(Pos2, Pos2)>,
    texts: Vec<(Pos2, Vec2, String)>,
}

impl Default for Painted {
    fn default() -> Painted {
        Painted {
            plate: Rect::NOTHING,
            nodes: Vec::new(),
            lines: Vec::new(),
            texts: Vec::new(),
        }
    }
}

impl Painted {
    fn words(&self) -> String {
        self.texts
            .iter()
            .map(|(_, _, words)| words.as_str())
            .collect::<Vec<&str>>()
            .join("\n")
    }
}

fn walk(shape: &egui::Shape, visit: &mut impl FnMut(&egui::Shape)) {
    visit(shape);
    if let egui::Shape::Vec(children) = shape {
        for child in children {
            walk(child, visit);
        }
    }
}

fn painted(output: &egui::FullOutput) -> Painted {
    let mut out = Painted::default();
    for clipped in &output.shapes {
        walk(&clipped.shape, &mut |shape| match shape {
            egui::Shape::Rect(rect) if rect.fill == PLATE => out.plate = rect.rect,
            egui::Shape::Circle(circle) if circle.fill == NODE => {
                out.nodes.push((circle.center, circle.radius))
            }
            egui::Shape::LineSegment { points, .. } => out.lines.push((points[0], points[1])),
            egui::Shape::Text(text) => out.texts.push((
                text.pos,
                text.galley.size(),
                text.galley.text().to_string(),
            )),
            _ => {}
        });
    }
    out
}

// --------------------------------------------------------------------------- the harness

/// What one press answered: the panel on the release frame, the canvas on the frame after it.
struct Click {
    canvas: Option<i64>,
    panel: Option<i64>,
}

/// What one frame answered and what it painted.
struct Drawn {
    canvas: Option<i64>,
    panel: Option<i64>,
    painted: Painted,
}

/// A headless page: one context, a clock, and a canvas size.
struct Page {
    ctx: egui::Context,
    time: f64,
    size: Vec2,
}

impl Page {
    fn new(size: Vec2) -> Page {
        let page = Page {
            ctx: egui::Context::default(),
            time: 0.0,
            size,
        };
        theme::install(&page.ctx);
        page
    }

    fn input(&mut self, events: Vec<egui::Event>) -> egui::RawInput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        self.time += 1.0 / 60.0;
        input
    }

    /// The canvas alone, at the page's full width: the frame the pointer is aimed at.
    fn canvas(&mut self, events: Vec<egui::Event>, view: &mut GraphView) -> Drawn {
        let height = self.size.y;
        let input = self.input(events);
        let mut canvas = None;
        let output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    canvas = view.show(ui, height);
                });
        });
        let painted = painted(&output);
        output.drop_without_applying_deltas();
        Drawn {
            canvas,
            panel: None,
            painted,
        }
    }

    /// The page as `app::Atlas` draws it: the canvas, then the panel beside it. The canvas is
    /// drawn first, so a click in the panel reaches `show` on the frame after it.
    fn page(&mut self, events: Vec<egui::Event>, view: &mut GraphView) -> Drawn {
        let height = self.size.y;
        let plate = (self.size.x - PANEL_WIDTH - 16.0).max(200.0);
        let input = self.input(events);
        let mut canvas = None;
        let mut panel_answer = None;
        let output = self.ctx.run_ui(input, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui(Vec2::new(plate, height), |ui| {
                            canvas = view.show(ui, height);
                        });
                        ui.allocate_ui(Vec2::new(PANEL_WIDTH, height), |ui| {
                            panel_answer = panel(ui, view);
                        });
                    });
                });
        });
        let drawn = painted(&output);
        output.drop_without_applying_deltas();
        Drawn {
            canvas,
            panel: panel_answer,
            painted: drawn,
        }
    }

    /// Press and release at one point — the gesture a reader makes to open something — and
    /// return the frame the release produced.
    fn click(&mut self, at: Pos2, view: &mut GraphView) -> Drawn {
        self.canvas(vec![egui::Event::PointerMoved(at)], view);
        self.canvas(button(at, true), view);
        self.canvas(button(at, false), view)
    }

    /// The same, on the page that has a panel.
    ///
    /// The panel answers on the release frame and the canvas on the frame after it, because
    /// the shell draws the canvas first: [`Click::canvas`] is read there, and it is the answer
    /// the shell actually keeps.
    fn click_page(&mut self, at: Pos2, view: &mut GraphView) -> Click {
        self.page(vec![egui::Event::PointerMoved(at)], view);
        self.page(button(at, true), view);
        let released = self.page(button(at, false), view);
        let after = self.page(Vec::new(), view);
        Click {
            panel: released.panel,
            canvas: after.canvas,
        }
    }
}

impl Page {
    /// Press and release at one point in two frames, for a sweep: the move arrives with the
    /// press (egui resolves the widget under the pointer first), and nothing is read back from
    /// the frame — the sweep reads the view after the release, which is where a click lands.
    fn press_release(&mut self, at: Pos2, view: &mut GraphView) {
        self.canvas(
            vec![egui::Event::PointerMoved(at), button_event(at, true)],
            view,
        );
        self.canvas(button(at, false), view);
    }

    /// Move the clock on without drawing: two presses this far apart are two clicks, not a
    /// double click, which would be a *reset* of the camera the sweep is aimed through.
    fn advance(&mut self, seconds: f64) {
        self.time += seconds;
    }

    /// Draw one frame with the pointer at `at` and answer what it painted: a hover promotes
    /// the grain under it, and a promoted grain is an obstacle for the names beside it.
    fn hover(&mut self, at: Pos2, view: &mut GraphView) -> Painted {
        self.canvas(vec![egui::Event::PointerMoved(at)], view).painted
    }

    /// Take the pointer off the plate and draw the frame.
    ///
    /// A pointer left where the last press landed hovers the dot under it, and a hovered dot is
    /// painted at its full radius instead of its grain: every aim measured on the frame before
    /// would then be measured against a circle that has grown. Off the plate, the picture is the
    /// picture the sweep enumerated.
    fn release_pointer(&mut self, view: &mut GraphView) {
        self.canvas(vec![egui::Event::PointerMoved(Pos2::new(-40.0, -40.0))], view);
    }
}

fn button_event(at: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

fn button(at: Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }]
}

/// Where the view says a node is painted: published camera, published layout.
fn projected(plate: Rect, view: &GraphView, slot: usize) -> Pos2 {
    let centre = plate.center() + view.pan;
    let node = view.nodes[slot];
    Pos2::new(centre.x + node.x * view.zoom, centre.y + node.y * view.zoom)
}

/// The entity a painted circle belongs to: the node whose projection is nearest.
fn slot_at(plate: Rect, view: &GraphView, at: Pos2) -> usize {
    let mut best = (usize::MAX, f32::INFINITY);
    for slot in 0..view.nodes.len() {
        let distance = projected(plate, view, slot).distance(at);
        if distance < best.1 {
            best = (slot, distance);
        }
    }
    best.0
}

// --------------------------------------------------------------------------- 1. the node

/// A press on a node selects that entity, and the panel then names it and lists what connects
/// it — the article list of an entity is its relations, and each of those is a door.
#[test]
fn a_node_click_selects_an_entity_that_has_relations_and_stories() {
    let Some((archive, path)) = live() else { return };
    let (mut view, entities, edges) = filled(&archive, &path);
    assert!(
        !view.nodes.is_empty(),
        "the graph is empty: {} entities and {} relations came out of {}",
        entities.len(),
        edges.len(),
        path.display()
    );
    let page = Page::new(Vec2::new(1100.0, 820.0));
    let mut page = page;

    let opening = page.canvas(Vec::new(), &mut view).painted;
    assert!(
        !opening.nodes.is_empty(),
        "the plate painted no node at all, so there is nothing to click"
    );
    // The strongest node, aimed at the centre of the circle the frame painted for it.
    let strongest = view.strongest()[0];
    let aim = projected(opening.plate, &view, strongest);
    let circle = opening
        .nodes
        .iter()
        .min_by(|left, right| left.0.distance(aim).total_cmp(&right.0.distance(aim)))
        .map(|(at, radius)| (*at, *radius))
        .expect("a painted circle");
    println!(
        "node click: slot {strongest} \"{}\" painted at {:?} radius {}",
        view.name_of(strongest),
        circle.0,
        circle.1
    );

    let drawn = page.click(circle.0, &mut view);
    assert_eq!(
        view.pick(circle.0, drawn.painted.plate.center() + view.pan),
        Some(strongest),
        "the press at {:?} did not resolve to the node painted there",
        circle.0
    );
    assert_eq!(
        view.selected,
        Some(strongest),
        "the press at the centre of the circle for slot {strongest} selected {:?}",
        view.selected
    );

    let name = view.name_of(strongest);
    assert!(!name.trim().is_empty(), "the selected entity has no name");
    let around = view.around(strongest);
    assert!(
        !around.is_empty(),
        "{name:?} is on the plate of a graph built from relations but has none"
    );
    println!(
        "node click: selected \"{name}\" ({} mentions) with {} relations, strongest first: {}",
        view.strength_of(strongest),
        around.len(),
        around
            .iter()
            .take(3)
            .map(|relation| format!("{} ({})", relation.name, relation.stories))
            .collect::<Vec<String>>()
            .join(", ")
    );

    // The destination the click leads to: the stories behind its strongest relation, each
    // one an article the archive still holds.
    let first = &around[0];
    let one = view.entities[strongest].id;
    let two = view.entities[first.other].id;
    let stories = stories_for(&path, one, two, STORIES_SHOWN).expect("a readable archive");
    assert!(
        !stories.is_empty(),
        "the strongest relation of {name:?} ({} with {:?}, {} stories) lists no story: the \
         row would open an empty list",
        first.stories,
        first.name,
        first.stories
    );
    let opened = archive.articles_by_ids(&[stories[0].id]);
    assert_eq!(
        opened.len(),
        1,
        "the story row offers article {} and the archive does not answer it",
        stories[0].id
    );
    assert!(
        !opened[0].title.trim().is_empty(),
        "article {} has no headline to show",
        stories[0].id
    );
    println!(
        "node click: {} -> {} resolves to {} stories; the first is {} {:?}",
        name,
        first.name,
        stories.len(),
        stories[0].id,
        stories[0].title
    );
}

// --------------------------------------------------------------------------- 2 + 3. the rows

/// A press on the first relation row of the selected node opens that relation: the row states
/// why the two are connected, the stories that name both ends follow it, and a press on one of
/// them answers the article it names.
///
/// Both presses are aimed at positions the panel painted — the row's own name, then the story's
/// own headline — so the claim is about the panel a reader sees, not about a row height this
/// file guessed.
#[test]
fn a_relation_row_opens_its_reason_and_a_story_row_opens_the_article() {
    let Some((archive, path)) = live() else { return };
    let (mut view, _entities, _edges) = filled(&archive, &path);
    let mut page = Page::new(Vec2::new(1100.0, 820.0));

    // Select the strongest node through a real press, so the panel below is the one a reader
    // would be looking at.
    let opening = page.canvas(Vec::new(), &mut view).painted;
    let strongest = view.strongest()[0];
    page.click(projected(opening.plate, &view, strongest), &mut view);
    assert_eq!(view.selected, Some(strongest), "nothing was selected to open");

    let around = view.around(strongest);
    assert!(!around.is_empty(), "the selected node lists no relation");
    // `Relation` is a borrowed row, not a value this file may keep: the two fields a press
    // needs are the name and the slot on the far end.
    let relation_name = around[0].name.clone();
    let expected = (
        strongest.min(around[0].other),
        strongest.max(around[0].other),
    );

    // The row's own name, as the panel painted it: the door is the whole box, so a press two
    // pixels inside its top-left corner is a press on the row.
    let first = page.page(Vec::new(), &mut view);
    let aim = first
        .painted
        .texts
        .iter()
        .find(|(at, _, words)| *words == relation_name && at.x > first.painted.plate.max.x)
        .map(|(at, _, _)| Pos2::new(at.x - 2.0, at.y + 3.0))
        .unwrap_or_else(|| {
            panic!(
                "the panel painted no row named {:?}: it says {:?}",
                relation_name,
                first.painted.words()
            )
        });
    let click = page.click_page(aim, &mut view);
    assert_eq!(
        click.canvas, click.panel,
        "the canvas and the panel answered differently for the press on the row: the shell \
         keeps the canvas's answer, so a row only one of them answers is a row that opens for a \
         reader and not for the page"
    );
    assert_eq!(
        view.focused(),
        Some(expected),
        "the press on the row {:?} opened {:?}; the row a reader sees first is {:?} ({:?})",
        relation_name,
        view.focused(),
        expected,
        view.name_of(expected.0)
    );
    println!(
        "relation row: the press at {aim:?} opened {:?} ({:?} -> {:?})",
        view.focused(),
        view.name_of(expected.0),
        view.name_of(expected.1)
    );

    // The reason, in plain words, painted under the name it belongs to.
    let reason =
        atlas_desktop::views::graph::relation_reason(&around[0].label, around[0].stories);
    assert!(
        !reason.trim().is_empty(),
        "the relation row states no reason at all"
    );
    let opened = page.page(Vec::new(), &mut view);
    let words = opened.painted.words();
    assert!(
        words.contains(&relation_name),
        "the opened row does not name {:?}: {words:?}",
        relation_name
    );
    assert!(
        words.contains(&reason),
        "the opened row does not state why ({reason:?}): the panel says {words:?}"
    );
    println!("relation row: reason {reason:?}");

    // The stories under the row: the ones that name both ends, and the first of them is a door
    // into the article it came from.
    let stories = stories_for(
        &path,
        view.entities[expected.0].id,
        view.entities[expected.1].id,
        STORIES_SHOWN,
    )
    .expect("a readable archive");
    assert!(
        !stories.is_empty(),
        "the relation the row opened has no story behind it"
    );
    let listed = stories.iter().any(|story| words.contains(&story.title));
    assert!(
        listed,
        "none of the {} stories behind the relation is painted under its row: {:?}",
        stories.len(),
        stories.iter().map(|story| &story.title).collect::<Vec<&String>>()
    );
    let story = &stories[0];
    let aim = opened
        .painted
        .texts
        .iter()
        .find(|(at, _, text)| *text == story.title && at.x > opened.painted.plate.max.x)
        .map(|(at, _, _)| Pos2::new(at.x - 2.0, at.y + 3.0))
        .unwrap_or_else(|| {
            panic!(
                "the panel painted no row for the story {:?}: it says {:?}",
                story.title, words
            )
        });
    let click = page.click_page(aim, &mut view);
    assert_eq!(
        click.panel,
        Some(story.id),
        "the press on the story row {:?} answered {:?}",
        story.title,
        click.panel
    );
    assert_eq!(
        click.canvas,
        Some(story.id),
        "the canvas answered {:?} for the story the panel opened on the frame before it",
        click.canvas
    );
    let article = archive.articles_by_ids(&[story.id]);
    assert_eq!(
        article.len(),
        1,
        "the story row offers article {} and the archive does not answer it",
        story.id
    );
    assert!(
        !article[0].title.trim().is_empty(),
        "article {} has no headline to show",
        story.id
    );
    println!(
        "story row: the press at {aim:?} opened article {} {:?}",
        story.id, article[0].title
    );
}

// --------------------------------------------------------------------------- 4. the line

/// A press on a relation line opens that relation — the two ends it joins, its reason, and its
/// stories, exactly as a press on the panel row does.
///
/// The line has to be a target on its own: the press is aimed 6 px clear of every painted circle,
/// so the node hit-test cannot be what answers, and a green claim here means the line itself was
/// found (`GraphView::pick_link`, `views/graph.rs:976`) and opened.
#[test]
fn a_click_on_a_relation_line_opens_the_pair_it_joins() {
    let Some((archive, path)) = live() else { return };
    let (mut view, _entities, _edges) = filled(&archive, &path);
    let mut page = Page::new(Vec2::new(1100.0, 820.0));

    // A line with room around its middle: no node within 40 px, so the press cannot be
    // answered by the node hit-test and be read as a line hit by accident.
    let drawn = page.canvas(Vec::new(), &mut view);
    let plate = drawn.painted.plate;
    let centre = plate.center() + view.pan;
    let mut chosen: Option<(Pos2, (usize, usize))> = None;
    for (one, two) in &drawn.painted.lines {
        let middle = *one + (*two - *one) * 0.5;
        // Six pixels clear of every circle as painted: close enough to be a line a reader aims
        // at, far enough that the node hit-test cannot be what answers.
        let clear = drawn
            .painted
            .nodes
            .iter()
            .all(|(at, radius)| at.distance(middle) > radius + 6.0);
        if !clear || view.pick(middle, centre).is_some() {
            continue;
        }
        let a = slot_at(plate, &view, *one);
        let b = slot_at(plate, &view, *two);
        if a != b {
            chosen = Some((middle, (a.min(b), a.max(b))));
            break;
        }
    }
    let Some((middle, pair)) = chosen else {
        panic!(
            "the plate painted {} lines and {} circles, and not one line has a middle that is \
             clear of every node: there is no line a reader could aim at",
            drawn.painted.lines.len(),
            drawn.painted.nodes.len()
        );
    };
    println!(
        "line click: aiming at {:?} for the pair {:?} ({:?} -> {:?})",
        middle,
        pair,
        view.name_of(pair.0),
        view.name_of(pair.1)
    );

    // Select something first: the defect is not only that the line is not a door, it is that a
    // press on it throws away the selection the reader had.
    let before = page.canvas(Vec::new(), &mut view).painted;
    page.click(projected(before.plate, &view, view.strongest()[0]), &mut view);
    assert!(
        view.selected.is_some(),
        "the harness could not select a node to lose"
    );

    page.click(middle, &mut view);
    assert_eq!(
        view.focused(),
        Some(pair),
        "a press on the line joining {:?} and {:?} (6 px clear of every painted circle) opened \
         {:?}, so the line itself is not what answered the press",
        view.name_of(pair.0),
        view.name_of(pair.1),
        view.focused()
    );
}

// --------------------------------------------------------------------------- 5. the name

/// A press on a painted name selects the node it names: the name is the label of an entity, and
/// a name a reader can read but not press is decoration.
///
/// The name is the most isolated one on the plate — its middle is farther from every painted
/// circle than any other's — so a press there can only be a press on the letters.
#[test]
fn a_click_on_a_name_selects_the_node_it_names() {
    let Some((archive, path)) = live() else { return };
    let (mut view, _entities, _edges) = filled(&archive, &path);
    let mut page = Page::new(Vec2::new(1100.0, 820.0));
    let drawn = page.canvas(Vec::new(), &mut view);

    // The most isolated name on the plate: the one whose middle is farthest from every painted
    // circle, which is the name a press can only be aimed at as a *name*.
    let mut chosen: Option<(Pos2, usize, String, f32)> = None;
    for (at, size, words) in &drawn.painted.texts {
        let middle = *at + *size * 0.5;
        let Some(slot) = (0..view.nodes.len()).find(|slot| view.name_of(*slot) == *words) else {
            continue;
        };
        let Some(nearest) = drawn
            .painted
            .nodes
            .iter()
            .map(|(node, radius)| node.distance(middle) - radius)
            .fold(None, |best: Option<f32>, gap| {
                Some(best.map_or(gap, |best| best.min(gap)))
            })
        else {
            continue;
        };
        if chosen.as_ref().map(|(_, _, _, gap)| nearest > *gap).unwrap_or(true) {
            chosen = Some((middle, slot, words.clone(), nearest));
        }
    }
    let Some((middle, slot, words, gap)) = chosen else {
        panic!(
            "the plate painted {} names and {} circles, and none of the names belongs to a node",
            drawn.painted.texts.len(),
            drawn.painted.nodes.len()
        );
    };
    println!(
        "name click: aiming at {middle:?} for {words:?} (slot {slot}), its middle is {gap} px \
         clear of every painted circle"
    );

    page.click(middle, &mut view);
    assert_eq!(
        view.selected,
        Some(slot),
        "a press on the name {words:?} selected {:?}",
        view.selected
    );
    println!("name click: the press selected \"{}\"", view.name_of(slot));
}

// --------------------------------------------------------------------------- 6. the sweep

/// The point of a painted segment that is farthest from every painted dot, and clear of every
/// painted name — a press inside a dot asks for the dot, and a press inside a name asks for the
/// name, so neither can measure whether the *line* is a target.
///
/// A line is drawn between two dot centres, so its ends are inside their own dots by
/// construction; the middle is where a reader aims. The clearance is measured, not assumed.
fn line_aim(segment: (Pos2, Pos2), painted: &Painted) -> (Pos2, f32) {
    let (one, two) = segment;
    let mut best = (one, f32::NEG_INFINITY);
    for step in 0..=24 {
        let at = one + (two - one) * (step as f32 / 24.0);
        let named = painted
            .texts
            .iter()
            .any(|(pos, size, _)| Rect::from_min_size(*pos, *size).contains(at));
        if named {
            continue;
        }
        let clear = painted
            .nodes
            .iter()
            .map(|(dot, radius)| dot.distance(at) - radius)
            .fold(f32::INFINITY, f32::min);
        if clear > best.1 {
            best = (at, clear);
        }
    }
    best
}

/// The point inside a painted name's box that is farthest from every painted dot. The letters
/// answer before a dot does, unless a *bigger* dot covers them.
fn clearest_in(box_: Rect, dots: &[(Pos2, f32)]) -> (Pos2, f32) {
    let mut best = (box_.center(), f32::NEG_INFINITY);
    for column in 1..5 {
        for row in 1..3 {
            let at = Pos2::new(
                box_.min.x + box_.width() * column as f32 / 5.0,
                box_.min.y + box_.height() * row as f32 / 3.0,
            );
            let clear = dots
                .iter()
                .map(|(dot, radius)| dot.distance(at) - radius)
                .fold(f32::INFINITY, f32::min);
            if clear > best.1 {
                best = (at, clear);
            }
        }
    }
    best
}

/// The box a frame painted a name in, if it painted that name at all.
fn name_box(painted: &Painted, words: &str) -> Option<Rect> {
    painted
        .texts
        .iter()
        .find(|(_, _, text)| text == words)
        .map(|(at, size, _)| Rect::from_min_size(*at, *size))
}

/// Is this pair a relation the archive stores, either direction? Cached: the answer is a fact
/// about the archive, not about the press, and the sweep asks it once per pair.
fn stored(
    archive: &Archive,
    cache: &mut HashMap<(i64, i64), bool>,
    one: i64,
    two: i64,
) -> bool {
    let key = (one.min(two), one.max(two));
    *cache.entry(key).or_insert_with(|| {
        archive
            .edges(&[key.0, key.1], 8)
            .iter()
            .any(|edge| (edge.a.min(edge.b), edge.a.max(edge.b)) == key)
    })
}

/// Everything the plate painted is a target: every relation line answers with the pair it
/// joins, and every name answers with the node it names. Not a sample — the picture the archive
/// draws at 1100×820, line by line and name by name.
///
/// Each aim is measured against the frame the pointer's own hover painted, not against the frame
/// from before it arrived: a hover promotes the grain under the pointer to full size, and a
/// promoted grain is an obstacle for the names beside it, so the picture a reader presses is the
/// picture under their pointer. The sweep also *reports* how many names move when the pointer
/// comes near — a name that jumps away from the aim is a name a reader can miss.
#[test]
fn every_painted_line_and_every_painted_name_is_a_target() {
    let Some((archive, path)) = live() else { return };
    let (mut view, _entities, _edges) = filled(&archive, &path);
    let mut page = Page::new(Vec2::new(1100.0, 820.0));
    let mut cache: HashMap<(i64, i64), bool> = HashMap::new();

    let first = page.canvas(Vec::new(), &mut view);
    let plate = first.painted.plate;
    let lines = first.painted.lines.clone();
    let names: Vec<(String, usize, Rect)> = first
        .painted
        .texts
        .iter()
        .filter_map(|(at, size, words)| {
            let slot = (0..view.nodes.len()).find(|slot| view.name_of(*slot) == *words)?;
            Some((words.clone(), slot, Rect::from_min_size(*at, *size)))
        })
        .collect();
    assert!(
        !lines.is_empty(),
        "the plate painted no relation line, so there is nothing to sweep"
    );
    assert!(
        !names.is_empty(),
        "the plate painted no name at all, so no name can be a target"
    );
    println!(
        "sweep: {} painted dots, {} painted lines, {} painted names on a {:?} plate",
        first.painted.nodes.len(),
        lines.len(),
        names.len(),
        plate.size()
    );

    // ---- every name ----
    let mut named = 0usize;
    // Every press that did not resolve to what the picture showed, with the value it resolved
    // to instead: one list for both sweeps, so one run reports the whole picture.
    let mut missed: Vec<String> = Vec::new();
    let mut drifted: Vec<String> = Vec::new();
    let mut name_gap = f32::INFINITY;
    for (words, slot, painted_box) in &names {
        assert!(
            !words.trim().is_empty(),
            "a painted name is empty, so nothing can be found by pressing it"
        );
        let (aim, clear) = clearest_in(*painted_box, &first.painted.nodes);
        name_gap = name_gap.min(clear);
        // Where the name is once the pointer arrives: a hover promotes the grain under it, and a
        // promoted grain is an obstacle for the names beside it, so the letters can move away
        // from the point a reader aimed at. Both facts are measured, not assumed.
        let hovered = page.hover(aim, &mut view);
        let after = name_box(&hovered, words);
        let holds = after.is_some_and(|box_| box_.contains(aim));
        if !holds {
            drifted.push(format!(
                "{words:?} from {painted_box:?} to {after:?} for an aim at {aim:?}"
            ));
        }
        page.release_pointer(&mut view);
        page.advance(0.4);
        page.press_release(aim, &mut view);
        if view.selected != Some(*slot) {
            missed.push(format!(
                "{words:?} (slot {slot}) at {aim:?} -> selected {:?}, opened {:?}",
                view.selected,
                view.focused()
            ));
        }
        named += 1;
        // Back to the picture the sweep enumerated, so the next name is aimed at its own box.
        page.release_pointer(&mut view);
        view.selected = None;
    }
    println!(
        "sweep: {named} names pressed ({} moved when the pointer arrived, worst clearance at an \
         aim {name_gap} px)",
        drifted.len()
    );
    for line in &drifted {
        println!("sweep: moved under the pointer: {line}");
    }

    // ---- every line ----
    let mut pressed = 0usize;
    let mut hidden = 0usize;
    let mut pairs: HashSet<(usize, usize)> = HashSet::new();
    for segment in &lines {
        let a = slot_at(plate, &view, segment.0);
        let b = slot_at(plate, &view, segment.1);
        assert_ne!(
            a, b,
            "a painted line goes from a node to itself at {segment:?}"
        );
        let pair = (a.min(b), a.max(b));
        pairs.insert(pair);
        let one = view.entities[pair.0].id;
        let two = view.entities[pair.1].id;
        let name_one = view.name_of(pair.0);
        let name_two = view.name_of(pair.1);
        assert!(
            !name_one.trim().is_empty() && !name_two.trim().is_empty(),
            "the line {pair:?} joins an entity with no name ({name_one:?}, {name_two:?})"
        );
        assert!(
            stored(&archive, &mut cache, one, two),
            "the line {pair:?} ({name_one:?} -> {name_two:?}) is not a relation the archive \
             stores ({one}, {two})"
        );

        // The same settle: the aim is re-chosen against the dots and names the hovered frame
        // painted, so a grain promoted by the pointer cannot swallow the aim on the way in.
        let mut aim = segment.0;
        let mut clear = f32::NEG_INFINITY;
        for _ in 0..1 {
            let hovered = page.hover(aim, &mut view);
            let (candidate, gap) = line_aim(*segment, &hovered);
            aim = candidate;
            clear = gap;
        }
        page.advance(0.4);
        let hit = view.pick_link(aim);
        page.press_release(aim, &mut view);
        if clear > 2.0 {
            // Clear of every dot and every name: the press can only be a press on the line.
            if hit != Some(pair) || view.focused() != Some(pair) || view.selected != Some(pair.0) {
                missed.push(format!(
                    "line {name_one:?} -> {name_two:?} at {aim:?} ({clear} px clear): the \
                     hit-test answered {hit:?}, the press opened {:?} and selected {:?}",
                    view.focused(),
                    view.selected
                ));
            }
            pressed += 1;
        } else {
            // No point of the line is outside the dots and names it runs between: it is a stub
            // between two dots that nearly touch, so the press must resolve to one of its own
            // ends — never to nothing, never to a third entity.
            if view.selected != Some(pair.0) && view.selected != Some(pair.1) {
                missed.push(format!(
                    "short line {name_one:?} -> {name_two:?} at {aim:?} selected {:?}, which is \
                     neither end",
                    view.selected
                ));
            }
            hidden += 1;
        }
        // Back to the picture the sweep enumerated before the next aim: the press left a
        // relation open, and an open relation draws its own line and its own two names.
        page.release_pointer(&mut view);
        view.selected = None;
    }
    println!(
        "sweep: {} lines pressed ({} distinct pairs), {pressed} answered as the line under the \
         pointer, {hidden} were shorter than their dots and answered as one of their ends",
        lines.len(),
        pairs.len()
    );
    assert_eq!(
        pressed + hidden,
        lines.len(),
        "the sweep pressed {} of {} painted lines",
        pressed + hidden,
        lines.len()
    );
    assert!(
        missed.is_empty(),
        "{} of the {} painted targets did not resolve to what they show: {missed:#?}",
        missed.len(),
        lines.len() + named
    );
}
