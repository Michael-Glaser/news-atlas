//! The native graph's geometry, pinned through the public `GraphView` API.
//!
//! Nothing here opens a window. `fill`, `tick`, `separate`, `drag_node` and `around` are
//! pure state, so the claims the picture makes about itself are checked as arithmetic:
//! the layout does not collapse, a lone entity keeps its distance, a relation's strength
//! is normalised, a node's relations are complete and ordered, and an edge to something
//! that is not in the entity list never becomes a line.
//!
//! The test links the library, `atlas_desktop::views::graph`, which is the module the
//! window draws from: the layout under test is the shipped one, and the two shapes `fill`
//! takes are `atlas_desktop::data`'s own rather than a copy that could keep passing while
//! the real `Entity` grew a field.

use atlas_desktop::data::{Edge, Entity};
use atlas_desktop::views::graph::GraphView;

fn entity(id: i64, count: i64) -> Entity {
    Entity { id, name: format!("E{id}"), kind: "PERSON".into(), count }
}

fn edge(a: i64, b: i64, weight: f64) -> Edge {
    Edge { a, b, weight, label: format!("{a} and {b}") }
}

fn centre_gap(view: &GraphView, a: usize, b: usize) -> f32 {
    let (one, two) = (view.nodes[a], view.nodes[b]);
    ((one.x - two.x).powi(2) + (one.y - two.y).powi(2)).sqrt()
}

/// The smallest `centre distance − (r1 + r2)` over every pair. Negative means two nodes
/// are drawn inside one another.
fn tightest(view: &GraphView) -> f32 {
    let mut tightest = f32::INFINITY;
    for a in 0..view.nodes.len() {
        for b in (a + 1)..view.nodes.len() {
            let clear = view.nodes[a].radius + view.nodes[b].radius;
            tightest = tightest.min(centre_gap(view, a, b) - clear);
        }
    }
    tightest
}

/// Tick until the cooling says the layout is at rest, which is the picture the reader
/// is given: `tick` stops itself at the same threshold, so the cap only exists to fail
/// loudly rather than spin.
fn settle(view: &mut GraphView) -> usize {
    let mut ticks = 0;
    while view.alpha > 0.004 && ticks < 2000 {
        view.tick();
        ticks += 1;
    }
    ticks
}

/// `n` entities in a loop — slot `i` joined to slot `i + 1` — with weights and mention
/// counts cycling, so the ring has both faint and firm lines and both small and large
/// radii.
fn ring(n: usize) -> (Vec<Entity>, Vec<Edge>) {
    let entities = (0..n)
        .map(|i| entity(i as i64 + 1, (i as i64 % 7) + 1))
        .collect();
    let edges = (0..n)
        .map(|i| {
            let a = i as i64 + 1;
            let b = ((i + 1) % n) as i64 + 1;
            edge(a, b, (i % 5) as f64 + 1.0)
        })
        .collect();
    (entities, edges)
}

#[test]
fn a_settled_layout_never_leaves_two_nodes_inside_each_other() {
    let mut view = GraphView::default();
    let (entities, edges) = ring(120);
    view.fill(entities, edges);
    let ticks = settle(&mut view);
    let margin = tightest(&view);
    assert!(
        margin >= 0.0,
        "after {ticks} ticks two nodes are {:.2} px inside each other",
        -margin
    );
    // and the picture is spread rather than a cloud on one spot: the layout covers
    // ground in both directions, with no node off the plane
    let (mut min_x, mut max_x, mut min_y, mut max_y) =
        (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
    for node in &view.nodes {
        assert!(node.x.is_finite() && node.y.is_finite(), "a node left the plane");
        min_x = min_x.min(node.x);
        max_x = max_x.max(node.x);
        min_y = min_y.min(node.y);
        max_y = max_y.max(node.y);
    }
    assert!(
        max_x - min_x > 100.0 && max_y - min_y > 100.0,
        "the layout collapsed to {} x {} px",
        max_x - min_x,
        max_y - min_y
    );
}

#[test]
fn a_pile_of_nodes_dragged_onto_one_point_is_pulled_apart() {
    // the degenerate case the forces cannot answer: at exactly the same coordinate
    // there is no direction to push along, so the separation pass has to invent one
    let mut view = GraphView::default();
    let (entities, edges) = ring(20);
    view.fill(entities, edges);
    let (x, y) = (view.nodes[0].x, view.nodes[0].y);
    for slot in 1..view.nodes.len() {
        let (sx, sy) = (view.nodes[slot].x, view.nodes[slot].y);
        view.drag_node(slot, egui::vec2(x - sx, y - sy));
    }
    assert!(tightest(&view) < 0.0, "the pile really is a pile");
    settle(&mut view);
    assert!(
        tightest(&view) >= 0.0,
        "{:.2} px of overlap survived the layout",
        -tightest(&view)
    );
}

#[test]
fn a_joined_pair_settles_closer_to_itself_than_to_an_isolated_entity() {
    // slot 0 is named by no relation; slots 1 and 2 are joined. A reader has to be able
    // to read that off the picture: the pair is one thing, the lone entity another.
    let mut view = GraphView::default();
    view.fill(
        vec![entity(1, 3), entity(2, 9), entity(3, 4)],
        vec![edge(2, 3, 6.0)],
    );
    settle(&mut view);
    let pair = centre_gap(&view, 1, 2);
    let nearest_isolated = centre_gap(&view, 0, 1).min(centre_gap(&view, 0, 2));
    assert!(
        pair < nearest_isolated,
        "the joined pair sits {pair:.1} apart, the isolated entity {nearest_isolated:.1} away"
    );
}

#[test]
fn the_strongest_relation_is_normalised_to_exactly_one() {
    let mut view = GraphView::default();
    view.fill(
        vec![entity(1, 3), entity(2, 5), entity(3, 2), entity(4, 8)],
        vec![edge(1, 2, 2.0), edge(1, 3, 9.0), edge(1, 4, 5.0), edge(2, 4, 1.0)],
    );
    let strongest = view
        .edges
        .iter()
        .map(|link| link.strength)
        .fold(f32::MIN, f32::max);
    assert_eq!(strongest, 1.0, "the picture's scale has to reach its own top");
    assert!(view.edges.iter().all(|link| (0.0..=1.0).contains(&link.strength)));

    // every other relation keeps its place on that scale — by slot, which is the order
    // the entities were handed over in
    let of = |a: usize, b: usize| {
        view.edges
            .iter()
            .find(|link| (link.a, link.b) == (a, b) || (link.a, link.b) == (b, a))
            .map(|link| link.strength)
            .unwrap_or(f32::NAN)
    };
    assert!((of(0, 2) - 1.0).abs() < 1e-6, "9 of 9: {}", of(0, 2));
    assert!((of(0, 3) - 5.0 / 9.0).abs() < 1e-6, "5 of 9: {}", of(0, 3));
    assert!((of(0, 1) - 2.0 / 9.0).abs() < 1e-6, "2 of 9: {}", of(0, 1));
    assert!((of(1, 3) - 1.0 / 9.0).abs() < 1e-6, "1 of 9: {}", of(1, 3));

    // strength is the picture; the stored weight is the fact behind it
    let heaviest = view
        .edges
        .iter()
        .find(|link| link.strength == 1.0)
        .expect("one relation is the strongest");
    assert_eq!(heaviest.stories, 9.0, "the count of stories is not thrown away");
}

#[test]
fn every_relation_of_a_node_is_listed_strongest_first() {
    let mut view = GraphView::default();
    view.fill(
        vec![entity(1, 3), entity(2, 5), entity(3, 2), entity(4, 8), entity(5, 1)],
        vec![
            // deliberately out of order, and not all of them touch slot 0
            edge(1, 2, 1.0),
            edge(1, 4, 4.0),
            edge(2, 3, 3.0),
            edge(1, 3, 9.0),
            edge(1, 5, 2.0),
        ],
    );
    let around = view.around(0);
    assert_eq!(around.len(), 4, "one row per relation, no more and no fewer");
    let names: Vec<&str> = around.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        names,
        ["E3", "E4", "E5", "E2"],
        "the rows go strongest first: {names:?}"
    );
    let strengths: Vec<f32> = around.iter().map(|row| row.strength).collect();
    assert!(
        strengths.windows(2).all(|pair| pair[0] > pair[1]),
        "the weights descend: {strengths:?}"
    );

    // the far end reads the same relation, and a node with no relations reads nothing
    assert_eq!(view.around(2).len(), 2, "E3 is joined to E1 and to E2");
    assert_eq!(view.around(2)[0].name, "E1", "and E1 is its strongest");
    assert_eq!(view.around(4).len(), 1, "E2's only relation");
    assert!(view.around(5).is_empty(), "nothing is recorded for an untouched entity");
    assert!(view.around(99).is_empty(), "a slot past the end is empty, not a panic");
}

#[test]
fn a_relation_to_an_entity_that_is_not_in_the_list_is_dropped() {
    let mut view = GraphView::default();
    view.fill(
        vec![entity(1, 3), entity(2, 5)],
        vec![
            edge(1, 2, 4.0),
            // relations to an entity the window did not load, in both directions
            edge(2, 999, 9.0),
            edge(999, 1, 8.0),
            // and an entity's relation to itself
            edge(1, 1, 7.0),
        ],
    );
    assert_eq!(
        view.edges.len(),
        1,
        "only the relation between two loaded entities survives"
    );
    assert_eq!((view.edges[0].a, view.edges[0].b), (0, 1));
    // the dropped 9.0 must not set the scale either: normalisation happens after the
    // entity list has had its say, so the line that is drawn is the strongest drawn
    assert_eq!(view.edges[0].stories, 4.0);
    assert_eq!(view.edges[0].strength, 1.0);
    assert_eq!(view.around(0).len(), 1);
    assert!(view.index.len() == 2, "the index holds the entities and nothing else");
}

#[test]
fn the_strongest_relation_is_the_strongest_even_when_every_weight_is_below_one() {
    // the scale used to be seeded at one story, so a window whose heaviest relation
    // weighed half of one drew its strongest line at half the palette
    let mut view = GraphView::default();
    view.fill(
        vec![entity(1, 3), entity(2, 5), entity(3, 2)],
        vec![edge(1, 2, 0.5), edge(2, 3, 0.25)],
    );
    assert_eq!(view.edges.len(), 2);
    assert_eq!(view.edges[0].stories, 0.5, "the stored weight is kept as it arrived");
    assert_eq!(view.edges[0].strength, 1.0, "half a story is the top of this scale");
    assert_eq!(view.edges[1].strength, 0.5, "the quarter is half of it");
    let strongest = view
        .edges
        .iter()
        .map(|link| link.strength)
        .fold(f32::MIN, f32::max);
    assert_eq!(strongest, 1.0, "the strongest drawn relation is the top of the scale");
}

#[test]
fn a_relation_stored_in_both_directions_is_one_row() {
    let mut view = GraphView::default();
    view.fill(
        vec![entity(1, 3), entity(2, 5), entity(3, 2)],
        vec![edge(1, 2, 4.0), edge(2, 1, 3.0), edge(2, 3, 1.0)],
    );
    assert_eq!(view.edges.len(), 2, "the mirrored pair is one relation, not two");
    assert_eq!(view.around(0).len(), 1, "E1's panel lists one row");
    assert_eq!(view.around(1).len(), 2, "E2 is joined to both, once each");
    assert_eq!(view.around(2).len(), 1);
    // the three views read one edge set: every relation has exactly two ends, so a row
    // counted twice anywhere would show up here
    let rows: usize = (0..3).map(|slot| view.around(slot).len()).sum();
    assert_eq!(rows, view.edges.len() * 2, "around, edges and degree agree");

    // the scale comes from the relation that was kept, at the weight it was stored with
    let kept = &view.edges[0];
    assert_eq!((kept.a, kept.b), (0, 1));
    assert_eq!(kept.stories, 4.0, "the weight the max was taken from");
    assert_eq!(kept.strength, 1.0);

    // and `degree` counted the pair once, so the strongest-first walk sees each node
    // once and puts the one with two relations first
    let order = view.strongest();
    let mut walked = order.clone();
    walked.sort_unstable();
    assert_eq!(walked, vec![0, 1, 2], "every node once");
    assert_eq!(order[0], 1, "E2 touches two relations, so it leads");

    // a heavier mirror arriving second is still not drawn: the kept row sets the scale,
    // so a weight no line shows cannot brighten the ones that are
    let mut other = GraphView::default();
    other.fill(
        vec![entity(1, 3), entity(2, 5)],
        vec![edge(1, 2, 3.0), edge(2, 1, 4.0)],
    );
    assert_eq!(other.edges.len(), 1);
    assert_eq!(other.edges[0].stories, 3.0);
    assert_eq!(other.edges[0].strength, 1.0, "the kept relation is the top of the scale");
}
