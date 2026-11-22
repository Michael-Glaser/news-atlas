//! The map's dots: what a cluster is made of, and what clicking one can open.
//!
//! A dot on the plate is a [`Cluster`]: the strongest pin behind it (`rep`), how many
//! places merged into it (`places`) and how many stories those places hold (`stories`).
//! Four things must be true at every zoom, and each is checked against the real archive,
//! read-only:
//!
//! * **nothing is lost.** Every pin on the plate is in exactly one dot — one place per dot,
//!   one dot per place — so every story of those places is counted once. The check is a
//!   replay of [`cluster`]'s own rule, and the replay is *not trusted on its own*: its
//!   `places`, `stories` and centre must equal the shipped dot's for every dot, so a change
//!   to the rule fails [`dots_at`] instead of quietly making this file agree with itself.
//! * **a click opens a story the archive holds.** The id a dot reports is a pin's newest
//!   story; it must be a real row, the same row the archive returns by key, and the dozens
//!   of dots on a plate must not report one story twice.
//! * **a dot never expands to nothing.** Every dot has at least one place, at least one
//!   story and at least one id; and a deeper zoom only ever *splits* a dot — each dot at a
//!   deeper zoom is contained in a single dot of the fit — so zooming in cannot invent a
//!   place, merge two places that were apart, or empty a dot.
//! * **a dot over one point is permanent.** The archive files several labels at one
//!   coordinate (country and capital), so those pins are never pulled apart by any zoom, and
//!   the story list of the dot has to carry both places' stories for the reader to reach them
//!   (`the_co_located_pairs_are_reachable_at_every_zoom`).
//! * **a dot names all of its stories, once.** The union of its members' story ids in the
//!   window is exactly `stories` — nothing dropped, nothing counted twice — and the list a
//!   press produces holds exactly those ids (`every_dot_names_all_of_its_stories`,
//!   `a_click_lists_exactly_the_stories_of_its_dot`).
//!
//! # The click, as the view now spells it
//!
//! [`Cluster::members`] landed (`src/views/map.rs:982`: every pin behind the dot, heaviest
//! first, `rep` among them), so a dot can name its places. A press answers a [`Pick`]
//! (`map.rs:1093`, set at `:1418`): `Pick::Places(members)` for a dot of several places,
//! `Pick::Place(rep)` for one. The card turns that into a story (`choice.story`, `rep`'s
//! newest) and, for a merged dot, one chip per member (`views/overview.rs:399
//! merged_places`), each chip opening that place's list through the product's own read —
//! `Door::place` → [`stories_for`] → `Archive::place_stories` (`data.rs:475`), capped at
//! [`LIST_ROWS`] (`app.rs:247`). The tests below drive exactly those calls, so "the list a
//! click produces" is the window's list and not a rule invented here.
//!
//! A single-place dot draws no chip of its own (one place is the story it already opened); its
//! place list is the chip the page draws under the map, and that list is the dot's own set of
//! stories — asserted, so a reader can reach them either way.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use atlas_desktop::app::{stories_for, LIST_ROWS};
use atlas_desktop::data::{Archive, Pin};
use atlas_desktop::geo::{Camera, Geo, Point};
use atlas_desktop::views::map::{cluster, Cluster, CLUSTER_PX};
use atlas_desktop::views::overview::Door;

use egui::{Pos2, Rect, Vec2};

/// The plate the Overview's map card allocates (`map::card` draws at 1 000 × 470), so a dot
/// is placed where the window would place it.
const PLATE: (f32, f32) = (1000.0, 470.0);
/// The three zoom levels the counts are reported at: the fit, where the whole world is on
/// the plate and every place with it, a working zoom, and one deep enough that a region is
/// all the plate shows. All three still merge places, so each one tests the rule.
const ZOOMS: [f32; 3] = [1.0, 2.0, 6.0];
/// The view's zoom ceiling (`geo::MAX_ZOOM`): a point that has not separated by here never
/// will.
const CEILING: f32 = 96.0;
/// More rows than any list holds, so a window is not what shapes a read.
const WIDE: i64 = 24 * 365 * 10;
const ALL: i64 = 10_000;

// ---------------------------------------------------------------- the archive

/// The checkout's archive: `rust/atlas-desktop` sits two directories below the root.
fn archive_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/atlas.db")
}

/// The live archive, or `None` on a checkout without one, with a line saying so.
fn live() -> Option<Archive> {
    let path = archive_path();
    if !path.is_file() {
        println!("skipped: no archive at {}", path.display());
        return None;
    }
    match Archive::open(&path) {
        Ok(archive) => Some(archive),
        Err(problem) => {
            println!("skipped: {problem}");
            None
        }
    }
}

// ------------------------------------------------------------- the plate

fn plate() -> Rect {
    Rect::from_min_size(Pos2::ZERO, Vec2::new(PLATE.0, PLATE.1))
}

/// The camera the view would hold: the fit, zoomed about the middle and then moved so
/// `centre` is under the middle of the plate.
fn camera_at(centre: Geo, zoom: f32) -> Camera {
    let mut camera = Camera::new(PLATE);
    camera.zoom_at(Point::new(0.0, 0.0), zoom);
    let point = camera.project(centre);
    camera.pan((-point.x, -point.y));
    camera
}

/// Where a place is drawn, in plate pixels — the closure `map::MapView::show` hands to
/// [`cluster`].
fn on_plate(camera: &Camera, centre: Pos2, pin: &Pin) -> Pos2 {
    let point = camera.project(Geo::new(pin.lon as f32, pin.lat as f32));
    Pos2::new(centre.x + point.x, centre.y + point.y)
}

/// Every pin drawn on the plate, in the archive's order, and where each lands.
fn placed(pins: &[Pin], camera: &Camera) -> Vec<(usize, Pos2)> {
    let rect = plate();
    let centre = rect.center();
    pins.iter()
        .enumerate()
        .map(|(index, pin)| (index, on_plate(camera, centre, pin)))
        .filter(|(_, point)| rect.contains(*point))
        .collect()
}

/// The dots the view would paint, and the places each one is made of.
///
/// The membership is a replay of [`cluster`]'s rule — heaviest first, joining the first dot
/// whose *moving* centre is within [`CLUSTER_PX`], a pin outside the plate dropped — and the
/// replay is checked against the shipped dots on every call: same count, same
/// representative, same `places`, same `stories`, same centre. If the rule in the view
/// changes, this fails rather than describing a map nobody draws.
fn dots_at(pins: &[Pin], camera: &Camera) -> (Vec<Cluster>, Vec<Vec<usize>>) {
    let rect = plate();
    let centre = rect.center();
    let at = |pin: &Pin| on_plate(camera, centre, pin);
    let dots = cluster(pins, at, rect, CLUSTER_PX);

    let mut order: Vec<usize> = (0..pins.len()).collect();
    order.sort_by(|left, right| pins[*right].count.cmp(&pins[*left].count).then(left.cmp(right)));
    let mut replayed: Vec<(usize, Pos2, i64, Vec<usize>)> = Vec::new();
    for index in order {
        let pin = &pins[index];
        let point = at(pin);
        if !rect.contains(point) {
            continue;
        }
        match replayed.iter_mut().find(|dot| dot.1.distance(point) < CLUSTER_PX) {
            Some(dot) => {
                dot.1 = (dot.1 + point.to_vec2()) / 2.0;
                dot.2 += pin.count;
                dot.3.push(index);
            }
            None => replayed.push((index, point, pin.count, vec![index])),
        }
    }

    assert_eq!(
        replayed.len(),
        dots.len(),
        "the replay and the view disagree on how many dots there are at zoom ×{:.1}",
        camera.zoom()
    );
    for (index, (dot, mine)) in dots.iter().zip(&replayed).enumerate() {
        assert_eq!(dot.rep, mine.0, "dot {index} reports another place than the replay");
        assert_eq!(dot.places, mine.3.len(), "dot {index} counts another number of places");
        assert_eq!(dot.stories, mine.2, "dot {index} counts another number of stories");
        assert!(
            dot.at.distance(mine.1) < 1e-4,
            "dot {index} is drawn at {:?}, the replay puts it at {:?}",
            dot.at,
            mine.1
        );
    }
    (dots, replayed.into_iter().map(|dot| dot.3).collect())
}

// ------------------------------------------------------------- the click

/// The dots' stories by pin index: the list each place's chip opens, uncapped.
///
/// [`Archive::place_stories`] is the read the product's chip uses, and it labels a place with
/// the expression [`Archive::pins`] groups by, so this is the same set of rows the pin
/// counted. Read with a window of years so the set is complete.
fn stories_of_place(archive: &Archive, pins: &[Pin]) -> Vec<BTreeSet<i64>> {
    pins.iter()
        .map(|pin| {
            archive
                .place_stories(&pin.place, WIDE, ALL)
                .into_iter()
                .map(|article| article.id)
                .collect()
        })
        .collect()
}

/// The story ids a press on `dot` puts in front of the reader.
///
/// Mirrors `views/overview.rs:369` and `:399`: the representative's newest story is
/// `choice.story`, and a dot of several places draws one chip per member, each chip opening
/// its place through [`Door::place`] and [`stories_for`] at the page's own cap.
fn click_list(archive: &Archive, pins: &[Pin], dot: &Cluster) -> Vec<i64> {
    let mut ids = vec![pins[dot.rep].article];
    if dot.members.len() > 1 {
        for member in &dot.members {
            let pin = &pins[*member];
            let door = Door::place(&pin.place, pin.count);
            ids.extend(
                stories_for(archive, &door, WIDE, LIST_ROWS)
                    .into_iter()
                    .map(|article| article.id),
            );
        }
    }
    ids
}

/// The pin pairs filed at one coordinate: the archive's country-and-capital labels, which no
/// projection can tell apart.
fn co_located(pins: &[Pin]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for (one, pin) in pins.iter().enumerate() {
        for other in one + 1..pins.len() {
            if pin.lat == pins[other].lat && pin.lon == pins[other].lon {
                pairs.push((one, other));
            }
        }
    }
    pairs
}

// ------------------------------------------------------------------ the proofs

#[test]
fn every_place_on_the_plate_is_in_exactly_one_dot() {
    let Some(archive) = live() else { return };
    let pins = archive.pins(WIDE, ALL);
    assert!(!pins.is_empty(), "the archive holds no located story");
    let busiest = Geo::new(pins[0].lon as f32, pins[0].lat as f32);

    for zoom in ZOOMS {
        let camera = camera_at(busiest, zoom);
        let rect = plate();
        let on_the_plate = placed(&pins, &camera);
        let (dots, members) = dots_at(&pins, &camera);

        // every pin on the plate appears in exactly one dot: the concatenation of the dots
        // is the on-plate pins, each once, with no pin invented and none dropped
        let mut seen: Vec<usize> = members.iter().flatten().copied().collect();
        seen.sort_unstable();
        let expected: Vec<usize> = on_the_plate.iter().map(|(index, _)| *index).collect();
        assert_eq!(seen, expected, "the dots at ×{zoom} are not a partition of the plate");

        // the numbers the view paints agree with the plate: no place and no story counted
        // twice, and none left out
        let places: usize = dots.iter().map(|dot| dot.places).sum();
        let stories: i64 = dots.iter().map(|dot| dot.stories).sum();
        let held: i64 = on_the_plate.iter().map(|(index, _)| pins[*index].count).sum();
        assert_eq!(places, on_the_plate.len(), "×{zoom} counted a place twice or dropped one");
        assert_eq!(stories, held, "×{zoom} counted a story twice or dropped one");

        for (index, dot) in dots.iter().enumerate() {
            assert!(dot.places >= 1 && dot.stories >= 1, "×{zoom} dot {index} is empty");
            assert_eq!(dot.places, members[index].len());
            assert_eq!(dot.rep, members[index][0], "the representative is not the strongest place");
            let counted: i64 = members[index].iter().map(|pin| pins[*pin].count).sum();
            assert_eq!(dot.stories, counted, "×{zoom} dot {index} miscounts its stories");
            assert!(
                rect.contains(dot.at),
                "×{zoom} dot {index} is drawn off the plate at {:?}",
                dot.at
            );
        }

        // two dots are never closer than the separation, which is the whole point of the
        // rule: a crowded coast is several dots, never one blob
        for (left, one) in dots.iter().enumerate() {
            for other in dots.iter().skip(left + 1) {
                assert!(
                    one.at.distance(other.at) >= CLUSTER_PX,
                    "×{zoom} has two dots {} px apart",
                    one.at.distance(other.at)
                );
            }
        }

        let merged = dots.iter().filter(|dot| dot.places > 1).count();
        let biggest = dots.iter().map(|dot| dot.places).max().unwrap_or(0);
        println!(
            "map dots at ×{zoom:<4} on {}: {} places on the plate, {} dots ({} single, {} merged, {} places in the biggest), {} stories",
            pins[0].place,
            on_the_plate.len(),
            dots.len(),
            dots.len() - merged,
            merged,
            biggest,
            stories
        );
    }
}

#[test]
fn a_click_reports_a_story_the_archive_holds() {
    let Some(archive) = live() else { return };
    let pins = archive.pins(WIDE, ALL);
    let busiest = Geo::new(pins[0].lon as f32, pins[0].lat as f32);

    for zoom in ZOOMS {
        let camera = camera_at(busiest, zoom);
        let (dots, members) = dots_at(&pins, &camera);

        // what the click reports: the representative pin's newest story
        let mut reported: Vec<i64> = Vec::new();
        for (index, dot) in dots.iter().enumerate() {
            let pin = &pins[dot.rep];
            assert!(pin.article > 0, "×{zoom} dot {index} at {} opens no story", pin.place);
            assert!(
                members[index].contains(&dot.rep),
                "×{zoom} dot {index} reports a place it does not hold"
            );
            let found = archive.articles_by_ids(&[pin.article]);
            assert_eq!(
                found.len(),
                1,
                "×{zoom} dot {index} at {} opens story {}, which the archive does not hold",
                pin.place,
                pin.article
            );
            assert_eq!(found[0].id, pin.article);
            reported.push(pin.article);
        }
        // two dots never open the same story: a story is filed at one place
        reported.sort_unstable();
        let mut unique = reported.clone();
        unique.dedup();
        assert_eq!(reported.len(), unique.len(), "×{zoom} has two dots claiming one story");

        // the ids the dot is made of — the list a click would offer if the view ever drew
        // one: every place it merged contributes its own newest story, and all are real rows
        for (index, dot) in dots.iter().enumerate() {
            let of_dot: Vec<i64> = members[index].iter().map(|pin| pins[*pin].article).collect();
            assert_eq!(
                of_dot.len(),
                dot.places,
                "×{zoom} dot {index} carries {} ids for {} places",
                of_dot.len(),
                dot.places
            );
            for id in &of_dot {
                assert!(*id > 0, "×{zoom} dot {index} carries story {id}");
            }
            let found = archive.articles_by_ids(&of_dot);
            assert_eq!(
                found.len(),
                of_dot.len(),
                "×{zoom} dot {index} at {} carries ids the archive does not hold: {of_dot:?}",
                pins[dot.rep].place
            );
        }
        let ids: usize = members.iter().flatten().count();
        println!(
            "map clicks at ×{zoom:<4}: {} dots report {} story ids, one per place of {ids} places on the plate",
            dots.len(),
            reported.len(),
        );
    }
}

#[test]
fn expanding_a_dot_never_empties_it_and_only_ever_splits() {
    let Some(archive) = live() else { return };
    let pins = archive.pins(WIDE, ALL);
    let busiest = Geo::new(pins[0].lon as f32, pins[0].lat as f32);

    // the fit: every place on the plate, so the fit is the widest a dot ever gets
    let (fit, fit_members) = dots_at(&pins, &camera_at(busiest, ZOOMS[0]));
    assert!(!fit.is_empty(), "the fit draws no dot at all");
    let mut which: Vec<usize> = vec![usize::MAX; pins.len()];
    for (index, members) in fit_members.iter().enumerate() {
        for pin in members {
            which[*pin] = index;
        }
    }

    for zoom in &ZOOMS[1..] {
        let (dots, members) = dots_at(&pins, &camera_at(busiest, *zoom));
        assert!(!dots.is_empty(), "×{zoom} draws no dot at all");
        for (index, dot) in dots.iter().enumerate() {
            // never empty: a dot always stands for at least one place and one story
            assert!(!members[index].is_empty(), "×{zoom} dot {index} expands to nothing");
            assert!(dot.places >= 1 && dot.stories >= 1);
            // and never a merge: everything this dot holds was one dot at the fit
            let home = which[members[index][0]];
            assert_ne!(home, usize::MAX, "×{zoom} holds a place the fit never drew");
            for pin in &members[index] {
                assert_eq!(
                    which[*pin], home,
                    "×{zoom} dot {index} merges places from two dots of the fit"
                );
            }
            assert!(
                fit[home].places >= members[index].len(),
                "×{zoom} dot {index} holds more places than the dot it came from"
            );
        }
        println!(
            "map expand ×{zoom:<4}: {} dots over {} places, none empty, each inside one of the fit's {} dots",
            dots.len(),
            members.iter().flatten().count(),
            fit.len()
        );
    }
}

/// The archive files a country and its capital at the same coordinate, so those pins overlap
/// exactly and no zoom can separate them: the dot counts their stories together. A member
/// list is what lets the reader reach the second one.
#[test]
fn pins_at_one_point_never_pull_apart() {
    let Some(archive) = live() else { return };
    let pins = archive.pins(WIDE, ALL);
    let pairs = co_located(&pins);
    assert!(!pairs.is_empty(), "no two pins share a point in this archive");

    for (one, other) in pairs {
        let here = Geo::new(pins[one].lon as f32, pins[one].lat as f32);
        assert_ne!(
            pins[one].article, pins[other].article,
            "{} and {} at one point report one story",
            pins[one].place, pins[other].place
        );
        for zoom in [ZOOMS[0], ZOOMS[2], CEILING] {
            let (dots, members) = dots_at(&pins, &camera_at(here, zoom));
            let left = members.iter().position(|dot| dot.contains(&one));
            let right = members.iter().position(|dot| dot.contains(&other));
            assert_eq!(
                left,
                right,
                "×{zoom} pulled {} and {} apart, which sit on one point",
                pins[one].place, pins[other].place
            );
            let dot = &dots[left.expect("the replay dropped a pin on the plate")];
            assert!(dot.places >= 2, "×{zoom} split a dot of one point into single places");
            assert_eq!(dot.rep, one, "the stronger place of a point is not the representative");
            assert!(
                dot.stories >= pins[one].count + pins[other].count,
                "×{zoom} dot at one point counts fewer stories than its places hold"
            );
            // more stories than the representative alone holds: the rest are only
            // reachable because the dot can name them
            assert!(
                dot.stories > pins[dot.rep].count,
                "×{zoom} dot at one point holds no more than its representative"
            );
            assert!(
                dot.members.contains(&one) && dot.members.contains(&other),
                "×{zoom} dot at one point does not name both places"
            );
        }
        println!(
            "map one point ×{}: {} ({} stories) and {} ({} stories) stay one dot of {} stories, named in its members",
            CEILING, pins[one].place, pins[one].count, pins[other].place, pins[other].count,
            pins[one].count + pins[other].count
        );
    }
}

/// A dot's `stories` is a number a reader is asked to trust, so it must be the size of a set
/// that exists in the archive: the union of the story ids its members' places hold in the
/// window. Every id counted once — no place's list overlaps another's — and every story of
/// every member present, so the number under the dot is not larger than what a click can
/// reach.
#[test]
fn every_dot_names_all_of_its_stories() {
    let Some(archive) = live() else { return };
    let pins = archive.pins(WIDE, ALL);
    // a label is one place: if two pins shared a label, a chip could open the other's stories
    for (one, pin) in pins.iter().enumerate() {
        for other in &pins[one + 1..] {
            assert_ne!(
                pin.place, other.place,
                "two pins carry the label {:?}, so one chip opens both",
                pin.place
            );
        }
    }
    // the door's list and the pin's count are the same set of rows; where they are not, the
    // equality below could not hold and this test would blame the dot for the read
    let of_place = stories_of_place(&archive, &pins);
    for (index, pin) in pins.iter().enumerate() {
        assert_eq!(
            of_place[index].len() as i64,
            pin.count,
            "the chip for {:?} opens {} stories, the pin counted {}",
            pin.place,
            of_place[index].len(),
            pin.count
        );
        assert!(
            pin.count <= LIST_ROWS,
            "{:?} holds {} stories, more than the page's own cap, so a chip list is short by {}",
            pin.place,
            pin.count,
            pin.count - LIST_ROWS
        );
    }
    let busiest = Geo::new(pins[0].lon as f32, pins[0].lat as f32);

    for zoom in ZOOMS {
        let (dots, members) = dots_at(&pins, &camera_at(busiest, zoom));
        let mut every: Vec<BTreeSet<i64>> = Vec::new();
        for (index, dot) in dots.iter().enumerate() {
            assert_eq!(dot.members, members[index], "×{zoom} dot {index} names other members");
            let mut union: BTreeSet<i64> = BTreeSet::new();
            for member in &dot.members {
                let held = &of_place[*member];
                let before = union.len();
                union.extend(held.iter().copied());
                assert_eq!(
                    union.len() - before,
                    held.len(),
                    "×{zoom} dot {index} counts a story of {:?} twice",
                    pins[*member].place
                );
            }
            assert_eq!(
                union.len() as i64,
                dot.stories,
                "×{zoom} dot {index} claims {} stories but its {} places hold {}",
                dot.stories,
                dot.places,
                union.len()
            );
            // the places' newest stories are inside their own lists: the destination a chip
            // opens is a story the dot counts
            for member in &dot.members {
                assert!(
                    union.contains(&pins[*member].article),
                    "×{zoom} dot {index} counts no story of {:?}",
                    pins[*member].place
                );
            }
            every.push(union);
        }
        // the dots' story sets are disjoint: a story is one place's, so two dots never both
        // count it
        for (left, one) in every.iter().enumerate() {
            for other in &every[left + 1..] {
                assert!(
                    one.is_disjoint(other),
                    "×{zoom} two dots count the same story: {:?}",
                    one.intersection(other).collect::<Vec<_>>()
                );
            }
        }
        let total: usize = every.iter().map(BTreeSet::len).sum();
        let claimed: i64 = dots.iter().map(|dot| dot.stories).sum();
        println!(
            "map sets at ×{zoom:<4}: {} dots claim {claimed} stories; their {} places hold {total} distinct ids",
            dots.len(),
            members.iter().flatten().count()
        );
    }
}

/// The list a press produces must be exactly the dot's stories, and the two kinds of dot
/// produce it in the two ways the card does (`views/overview.rs:369`): a dot of several
/// places draws one chip per member (`:399`), so its press lists every story it counts; a dot
/// of one place opens that place's newest story, and the place's own chip under the map is the
/// list of the rest — which is the same set of rows the dot counted.
#[test]
fn a_click_lists_exactly_the_stories_of_its_dot() {
    let Some(archive) = live() else { return };
    let pins = archive.pins(WIDE, ALL);
    let of_place = stories_of_place(&archive, &pins);
    let busiest = Geo::new(pins[0].lon as f32, pins[0].lat as f32);

    for zoom in ZOOMS {
        let (dots, _) = dots_at(&pins, &camera_at(busiest, zoom));
        let mut reached = 0usize;
        let mut merged = 0usize;
        for (index, dot) in dots.iter().enumerate() {
            // what the dot counts: its members' stories
            let mut counted: BTreeSet<i64> = BTreeSet::new();
            for member in &dot.members {
                counted.extend(of_place[*member].iter().copied());
            }
            assert_eq!(counted.len() as i64, dot.stories);
            let clicked = click_list(&archive, &pins, dot);
            let listed: BTreeSet<i64> = clicked.iter().copied().collect();
            // the story the press opens is the representative's newest, and the dot counts it
            assert_eq!(clicked[0], pins[dot.rep].article);
            assert!(counted.contains(&clicked[0]));
            if dot.members.len() > 1 {
                merged += 1;
                // one chip per member: the press lists every story the dot counts, and
                // nothing it does not
                assert!(
                    listed.is_subset(&counted),
                    "×{zoom} dot {index} shows ids it does not count: {:?}",
                    listed.difference(&counted).collect::<Vec<_>>()
                );
                assert_eq!(
                    listed.len() as i64,
                    dot.stories,
                    "×{zoom} dot {index} at {} counts {} stories and the press lists {}",
                    pins[dot.rep].place,
                    dot.stories,
                    listed.len()
                );
                for member in &dot.members {
                    let pin = &pins[*member];
                    let door = Door::place(&pin.place, pin.count);
                    let opened = stories_for(&archive, &door, WIDE, LIST_ROWS);
                    assert_eq!(
                        opened.len() as i64,
                        pin.count,
                        "×{zoom} the chip for {:?} opens {} of {} stories",
                        pin.place,
                        opened.len(),
                        pin.count
                    );
                    assert_eq!(
                        opened.first().map(|article| article.id),
                        Some(pin.article),
                        "×{zoom} the chip for {:?} does not open its newest story",
                        pin.place
                    );
                }
                reached += listed.len();
            } else {
                // a dot of one place: the id it opens, and the place's own chip is the list
                assert_eq!(clicked.len(), 1, "×{zoom} a dot of one place shows more than one id");
                let pin = &pins[dot.rep];
                let door = Door::place(&pin.place, pin.count);
                let opened = stories_for(&archive, &door, WIDE, LIST_ROWS);
                let below: BTreeSet<i64> = opened.iter().map(|article| article.id).collect();
                assert_eq!(
                    below, counted,
                    "×{zoom} the chip for {:?} below the map is not the dot's stories",
                    pin.place
                );
                reached += below.len();
            }
        }
        println!(
            "map press ×{zoom:<4}: {} dots ({merged} merged) reach {reached} ids, exactly the stories they count",
            dots.len()
        );
    }
}

/// The co-located pairs — a country and its capital on one point — can never be separated by
/// the projection, so the *list* is the only way to the second one's stories. At every zoom,
/// including the fit and the ceiling, the press must offer both places and both must open.
#[test]
fn the_co_located_pairs_are_reachable_at_every_zoom() {
    let Some(archive) = live() else { return };
    let pins = archive.pins(WIDE, ALL);
    let of_place = stories_of_place(&archive, &pins);
    let pairs = co_located(&pins);
    assert!(!pairs.is_empty(), "no two pins share a point in this archive");

    // the two the reader complained about, named so a regression cannot pass by counting
    for named in [["China", "Beijing"], ["United States", "Washington"]] {
        let found = pairs.iter().any(|(one, other)| {
            let places = [pins[*one].place.as_str(), pins[*other].place.as_str()];
            named.iter().all(|want| places.contains(want))
        });
        assert!(found, "the archive no longer files {named:?} on one point");
    }

    for (one, other) in pairs {
        let here = Geo::new(pins[one].lon as f32, pins[one].lat as f32);
        for zoom in [ZOOMS[0], ZOOMS[1], ZOOMS[2], CEILING] {
            let (dots, members) = dots_at(&pins, &camera_at(here, zoom));
            let index = members
                .iter()
                .position(|dot| dot.contains(&one))
                .expect("the replay dropped a pin on the plate");
            let dot = &dots[index];
            assert!(
                dot.members.contains(&other),
                "×{zoom} {} and {} are in different dots",
                pins[one].place,
                pins[other].place
            );
            // a dot of several places is what draws the chips, so this pair has a list
            assert!(
                dot.members.len() > 1,
                "×{zoom} the dot at one point names one place, so nothing lists the other"
            );
            let listed: BTreeSet<i64> = click_list(&archive, &pins, dot).into_iter().collect();
            for member in [one, other] {
                let pin = &pins[member];
                assert!(
                    listed.contains(&pin.article),
                    "×{zoom} the list of the dot does not hold the newest story of {:?}",
                    pin.place
                );
                for id in &of_place[member] {
                    assert!(
                        listed.contains(id),
                        "×{zoom} the list of the dot drops story {id} of {:?}",
                        pin.place
                    );
                }
            }
            assert_eq!(
                listed.len() as i64,
                dot.stories,
                "×{zoom} the pair's dot counts {} stories and its list holds {}",
                dot.stories,
                listed.len()
            );
        }
        println!(
            "map pair ×{}: {} ({} stories) + {} ({} stories) reachable through one dot's list",
            CEILING, pins[one].place, pins[one].count, pins[other].place, pins[other].count
        );
    }
}
