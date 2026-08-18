mod cargo;
mod docker;
mod homebrew;
mod node;
mod spotify;
mod time_machine;
mod trash;

use crate::core::{Module, Registry};

pub fn all() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(spotify::SpotifyModule),
        Box::new(homebrew::HomebrewModule),
        Box::new(docker::DockerModule),
        Box::new(cargo::CargoModule),
        Box::new(node::NodeModule),
        Box::new(time_machine::TimeMachineModule),
        Box::new(trash::TrashModule),
    ]
}

/// The only place that names the built-in modules. Core takes a
/// [`Registry`] and does not import this crate path.
pub fn registry() -> Registry {
    Registry::new(all())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_ids_are_unique_and_include_trash() {
        let modules = all();
        let mut seen = std::collections::HashSet::new();
        for module in &modules {
            assert!(
                seen.insert(module.id()),
                "duplicate module id '{}'",
                module.id()
            );
        }
        assert!(seen.contains("trash"));
        assert_eq!(modules.len(), seen.len());
    }

    #[test]
    fn core_allow_list_comes_from_modules() {
        let registry = registry();
        let programs = registry.programs();
        assert!(programs.contains(&"docker"));
        assert!(programs.contains(&"tmutil"));
        assert!(!programs.contains(&"osascript"));
        assert!(!programs.iter().any(|p| *p == "rm"));
        let dirs = registry.path_dirs();
        assert!(dirs.contains(&"/Applications/Docker.app/Contents/Resources/bin"));
    }

    #[test]
    fn schedule_targets_are_unique_and_namespaced() {
        let registry = registry();
        let mut seen = std::collections::HashSet::new();
        for target in registry.schedule_targets() {
            let module = target.id.split(':').next().unwrap_or("");
            assert!(
                registry.get(module).is_some(),
                "selector '{}' does not belong to a module",
                target.id
            );
            assert!(
                crate::schedule::valid_selector(target.id),
                "selector '{}' is not a valid job id",
                target.id
            );
            assert!(
                seen.insert(target.id),
                "duplicate schedule target '{}'",
                target.id
            );
        }
        assert!(seen.contains("cargo:projects"));
        assert!(!seen.iter().any(|id| id.starts_with("timemachine:")));
    }
}
