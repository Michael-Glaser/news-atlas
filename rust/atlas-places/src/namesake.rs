//! Which of two same-named cities a text means.
//!
//! Decides: when a name belongs to cities in more than one country ("Valencia"
//! in Spain and in Venezuela, "Tripoli" in Libya and in Lebanon), which of them
//! this text means — and, when the text does not say, that it does not say.
//!
//! The decision is made from the *rest of the candidate list*, never from a word
//! list: whichever namesake's country the text also puts evidence behind (its
//! country named, another of its cities, its adjective) is the one meant. With no
//! such evidence the outlet's own country is the next weakest honest signal, and
//! with neither, nothing is claimed: every namesake is reported as undecided and
//! the story is left unplaced rather than pinned to a country by table order.
//!
//! Does not decide: how a mention is found (`crate::scan`), how one mention is
//! weighted or the zones combine (`crate::score`), or any byte of the answer
//! (`crate::report`).

use crate::score::Cand;

/// The text does not say which of the same-named cities it means.
const UNDECIDED: &str = "a same-named city exists in another country and the text does not say which";

/// This namesake lost: the text points at a city of the same name elsewhere.
const OVERRULED: &str = "a same-named city elsewhere; the text supports that one";

/// This namesake lost: the outlet's country is the other one.
const OUTLET_SAID: &str = "a same-named city elsewhere; the outlet's country chose that one";

/// A city decided by its outlet's country is a weaker claim than a text that
/// names the country: its confidence may not read as if the text had said it.
const OUTLET_CONF_CAP: f64 = 0.5;

/// One shared name: the candidates that carry it, in list order.
struct Shared {
    members: Vec<usize>,
}

/// Resolve every shared name in `cands`, in place.
///
/// `cands[i].note` is empty for a candidate still in the running and the fate of
/// one that is out; only candidates still in the running are ever resolved, and
/// only they count as evidence for a namesake's country.
pub(crate) fn resolve(cands: &mut [Cand], outlet_iso: &str) {
    let mut shared: Vec<Shared> = Vec::new();
    for i in 0..cands.len() {
        if cands[i].kind != crate::index::Kind::City || !cands[i].note.is_empty() {
            continue;
        }
        match shared.iter_mut().find(|s| cands[s.members[0]].place == cands[i].place) {
            Some(s) => {
                if !s.members.iter().any(|&m| cands[m].iso == cands[i].iso) {
                    s.members.push(i);
                }
            }
            None => shared.push(Shared { members: vec![i] }),
        }
    }
    for group in shared.iter().filter(|s| s.members.len() > 1) {
        decide(cands, &group.members, outlet_iso);
    }
}

fn decide(cands: &mut [Cand], members: &[usize], outlet_iso: &str) {
    // How much of the text, outside this name, is about each namesake's country.
    let mut support: Vec<f64> = Vec::with_capacity(members.len());
    for &m in members {
        let iso = cands[m].iso;
        let sum: f64 = cands
            .iter()
            .enumerate()
            .filter(|(i, c)| !members.contains(i) && c.note.is_empty() && c.iso == iso)
            .map(|(_, c)| c.score)
            .sum();
        support.push(sum);
    }

    let best = support.iter().cloned().fold(0.0f64, f64::max);
    if best > 0.0 {
        let winners: Vec<usize> = (0..support.len()).filter(|&k| support[k] == best).collect();
        if winners.len() == 1 {
            let winner = members[winners[0]];
            for &m in members {
                if m != winner {
                    cands[m].note = OVERRULED;
                }
            }
            return;
        }
        // Two countries the text supports equally: still no answer.
        for &m in members {
            cands[m].note = UNDECIDED;
        }
        return;
    }

    // No country evidence at all: the outlet is the only signal left.
    let by_outlet: Vec<usize> = (0..support.len())
        .filter(|&k| !outlet_iso.is_empty() && cands[members[k]].iso == outlet_iso)
        .collect();
    if by_outlet.len() == 1 {
        let winner = members[by_outlet[0]];
        for &m in members {
            if m != winner {
                cands[m].note = OUTLET_SAID;
            }
        }
        cands[winner].conf_cap = cands[winner].conf_cap.min(OUTLET_CONF_CAP);
        return;
    }
    for &m in members {
        cands[m].note = UNDECIDED;
    }
}
