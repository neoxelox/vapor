//! The scenario catalog. `all()` is the source of truth for
//! `vapor-e2e list` and the docs; ids are stable, order is the run
//! order. One scenario proves one behavior.

use crate::scenario::Scenario;

pub mod basics;
pub mod cli;
pub mod modes;
pub mod resilience;
pub mod service;
pub mod structure;
pub mod sync;

pub fn all() -> Vec<Scenario> {
    let mut list = Vec::new();
    list.extend(basics::scenarios());
    list.extend(sync::scenarios());
    list.extend(cli::scenarios());
    list.extend(structure::scenarios());
    list.extend(resilience::scenarios());
    list.extend(modes::scenarios());
    list.extend(service::scenarios());
    // Stable run order: S scenarios by id, then the R phase.
    list.sort_by_key(|scenario| (scenario.id.starts_with('R'), scenario.id));
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_ids_and_names_are_unique() {
        let all = all();
        let mut ids = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();
        for scenario in &all {
            assert!(ids.insert(scenario.id), "duplicate id {}", scenario.id);
            assert!(
                names.insert(scenario.name),
                "duplicate name {}",
                scenario.name
            );
            assert!(
                !scenario.proves.is_empty(),
                "{} has no proves line",
                scenario.id
            );
        }
    }
}
