//! The scenario catalog. `all()` is the source of truth for
//! `vapor-vm list` and the docs; ids are stable, and one scenario
//! proves one behaviour.

use crate::scenario::Scenario;

pub mod app;
pub mod install;
pub mod states;

pub fn all() -> Vec<Scenario> {
    let mut list = Vec::new();
    list.extend(install::scenarios());
    list.extend(app::scenarios());
    list.extend(states::scenarios());
    list.sort_by_key(|scenario| scenario.id);
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
                "{} proves nothing",
                scenario.id
            );
        }
    }
}
