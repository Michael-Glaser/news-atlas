//! One real story from the archive, entity by entity.
//!
//! Article 3444 of `data/atlas.db` — "Berlin Summit: Europe Ready To Talk To Putin, But
//! Not Sans Ukraine" — is the fixture the entity audit was measured on: its stored rows
//! carried "Berlin Summit Europe Ready", "But Not Sans Ukraine", "Europe Ready To Talk"
//! and "Chancellor Merz" as entities, and typed Berlin/Kyiv/Moscow as untyped, because a
//! headline is one long capitalised run and a summary's title-plus-surname is two
//! capitalised words. The text below is the page's own stored `content`, word for word
//! (a newsletter page, so its subheadlines are glued onto the sentences — that is what
//! makes the runs cross, and it is what the old rules tripped over).
//!
//! The engines are the shipped ones, in this process (`atlas-core` and `atlas-places`
//! compiled in): no binary, no model, no network.

use atlas_ingest::entities::{extract, places};
use atlas_ingest::rules::NativeRules;

const TITLE: &str = "Berlin Summit: Europe Ready To Talk To Putin, But Not Sans Ukraine";
const SUMMARY: &str = "Putin invited European leaders to Moscow to discuss Ukraine, but Chancellor Merz insisted Europe can only join talks alongside the US, Russia and Ukraine. The 36-country Coalition of the Willing in Berlin pledged additional support to Kyiv, including 45 billion Pounds from the EU and 10 billion Euros from the UK.";
const BODY: &str = "Putin said Russia invited Europeans to Moscow to set out their position on Ukraine, without committing to a formal role for Europe in peace talks. Merz said Europe is ready for talks with Russia only if it joins the US, Russia and Ukraine at the table. The 36-country Coalition of the Willing in Berlin pledged more support for Kyiv, including 45 billion Pound from the EU and 10 billion Euros from the UK.An invitation reached Europe from Moscow on Thursday, just as the continent’s leaders sat down in Berlin to plan what a peacekeeping force in Ukraine might look like. Vladimir Putin said Russia had invited Europeans to Moscow to set out their position on Ukraine. Chancellor Friedrich Merz replied within hours that Europe is prepared for talks with Russia, but only if it is given a seat at the table alongside the US, Russia and Ukraine. The offer came as Russia kept up its attacks on Kyiv. Finland Detects Strike On Russian Warship In Baltic Sea Amid Ukraine Peace TalksTwo Invitations, One ChairPutin’s offer is narrower than it sounds. He said the US side was pressing Moscow “insistently” to accept European participation in the talks. But he did not commit to giving Europeans a formal role in a peace process, and the Kremlin declined to say which leaders had been invited. Jared Kushner, Trump’s son-in-law, said last week that Putin had agreed to try to let Europeans take part in talks, which were last held in Abu Dhabi. What Moscow wants answered, Putin made clear, is the question of troops: Europe’s plan to put forces on Ukrainian soil once the fighting ends. China Confirms Trump-Putin-Xi Meeting At APEC Summit in ShenzhenHe put the blame for the lack of a deal on Kyiv and its backers, saying Russia had accepted US compromises, including those from the Anchorage summit, “exactly as they were presented”. Asked whether the Shenzhen summit was a new Yalta, he answered with a question: what was bad about Yalta? ICJ Rules Myanmar Committed Rohingya Genocide: What It Means For Israel, Gaza And UkraineBerlin’s Answer, In Euros And TroopsIn Berlin, the 36 countries of the Coalition of the Willing, led by the UK, France and Germany, pledged more financial and military support for Ukraine, in a meeting timed to coincide with the Shenzhen summit. The EU is readying 45 billion Pounds for Ukraine’s funding needs next year, and the UK promised 10 billion Euros. Merz hosted Volodymyr Zelensky and the leaders of the UK and France, and the group discussed a Multinational Force for Ukraine to deploy once the fighting ends. US envoys Steve Witkoff and Kushner attended, bringing a prosperity plan for Ukraine’s reconstruction and a briefing on Trump’s talks in China. European Commission President Ursula von der Leyen said peace depends on putting Ukraine in a position of strength, and that a 22nd sanctions package was on the way. European Council President António Costa said the priority was pressure on Russia to come to the table, and that questions concerning Europe should be decided by Europe. EU leaders take up the funding proposal next week. From Courting Allies To Sitting With Rivals: Five Takeaways From Trump's Asia TourEurope has its conditions. So does Moscow. Each has sent an invitation with its terms printed on the back.";

fn rules() -> NativeRules {
    let config = atlas_ingest::Config::for_root(std::env::temp_dir());
    NativeRules::discover(&config).expect("the engines answer in-process")
}

#[test]
fn the_stored_story_loses_the_names_nobody_read() {
    let names = extract(&rules(), TITLE, SUMMARY, BODY, Some("Ukraine"));
    let listed: Vec<&str> = names.iter().map(|(name, _)| name.as_str()).collect();
    for junk in [
        "Berlin Summit Europe Ready",
        "But Not Sans Ukraine",
        "Europe Ready To Talk",
        "Chancellor Merz",
    ] {
        assert!(
            !listed.contains(&junk),
            "{junk} is a headline fragment, not a name: {listed:?}"
        );
    }
    for name in ["Berlin", "Ukraine", "Russia", "Merz"] {
        assert!(
            listed.iter().any(|found| *found == name),
            "{name} was lost: {listed:?}"
        );
    }
    for name in ["Berlin", "Ukraine", "Russia", "Kyiv", "Moscow"] {
        let kind = names
            .iter()
            .find(|(found, _)| found == name)
            .map(|(_, kind)| kind.as_str());
        assert_eq!(kind, Some("LOC"), "{name} should be a place: {names:?}");
    }
    // A person is not typed by the number of his words: `MISC` until a model says.
    assert_eq!(
        names
            .iter()
            .find(|(found, _)| found == "Merz")
            .map(|(_, kind)| kind.as_str()),
        Some("MISC")
    );
}

#[test]
fn the_stored_story_is_on_the_map_in_more_than_one_country() {
    let found = places(&rules(), TITLE, SUMMARY, BODY);
    let mut isos: Vec<&str> = found.iter().map(|place| place.iso.as_str()).collect();
    isos.sort_unstable();
    isos.dedup();
    // The story names Ukraine, Russia and Germany, and more; the map has to be able to
    // put it in each of them, not only in the one place `locate` chose as primary.
    for iso in ["UKR", "RUS", "DEU"] {
        assert!(isos.contains(&iso), "{iso} is named and missing: {isos:?}");
    }
    assert!(
        found.iter().all(|place| place.iso.len() == 3),
        "a row for a non-ISO answer would be a country the map cannot draw: {found:?}"
    );
    // "UK" is a two-letter name, which `atlas-graph`'s three-character name floor would
    // refuse — it is trusted because this crate hands it over as a `LOC` place, which is
    // the same door "New Zealand" uses. The story says "the UK" four times.
    assert!(
        found
            .iter()
            .any(|place| place.name == "UK" && place.iso == "GBR"),
        "the two-letter country name must still reach the map: {found:?}"
    );
}
