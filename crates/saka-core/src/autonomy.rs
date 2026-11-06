use crate::model::Task;
use crate::vocab::{Autonomy, WorkItemOrigin};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ceilings {
    pub default: Autonomy,
    pub repos: Vec<(String, Autonomy)>,
}

impl Default for Ceilings {
    fn default() -> Ceilings {
        Ceilings {
            default: Autonomy::Standard,
            repos: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Levels {
    pub requested: Option<Autonomy>,
    pub allowed: Autonomy,
    pub effective: Autonomy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoPolicy {
    pub full_name: String,
    pub requested: Option<Autonomy>,
}

pub fn effective(allowed: Autonomy, requested: Option<Autonomy>) -> Autonomy {
    requested.map_or(allowed, |r| r.min(allowed))
}

impl Ceilings {
    pub fn allowed(&self, full_name: &str) -> Autonomy {
        self.repos
            .iter()
            .find(|(repo, _)| repo.eq_ignore_ascii_case(full_name))
            .map_or(self.default, |(_, level)| *level)
    }

    pub fn levels(&self, full_name: &str, requested: Option<Autonomy>) -> Levels {
        let allowed = self.allowed(full_name);
        Levels {
            requested,
            allowed,
            effective: effective(allowed, requested),
        }
    }
}

impl Task {
    pub fn needs_start_approval(&self, ceilings: &Ceilings) -> bool {
        self.item.origin == WorkItemOrigin::SpecStep
            && self.item.start_approved_at.is_none()
            && self.autonomy(ceilings) == Autonomy::Strict
    }

    pub fn autonomy(&self, ceilings: &Ceilings) -> Autonomy {
        ceilings
            .levels(&self.policy.full_name, self.policy.requested)
            .effective
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Autonomy; 3] = [Autonomy::Strict, Autonomy::Standard, Autonomy::Autonomous];

    #[test]
    fn the_effective_level_is_the_lowest_of_the_ceiling_and_the_request() {
        for allowed in ALL {
            assert_eq!(effective(allowed, None), allowed);
            for requested in ALL {
                assert_eq!(effective(allowed, Some(requested)), allowed.min(requested));
            }
        }
        assert_eq!(
            effective(Autonomy::Standard, Some(Autonomy::Autonomous)),
            Autonomy::Standard,
            "a repo file never raises the operator's ceiling"
        );
    }

    #[test]
    fn a_repo_listed_by_the_operator_overrides_the_default_without_regard_to_case() {
        let ceilings = Ceilings {
            default: Autonomy::Standard,
            repos: vec![("Acme/API".to_string(), Autonomy::Autonomous)],
        };
        assert_eq!(ceilings.allowed("acme/api"), Autonomy::Autonomous);
        assert_eq!(ceilings.allowed("acme/web"), Autonomy::Standard);
        let levels = ceilings.levels("acme/api", Some(Autonomy::Strict));
        assert_eq!(
            (levels.requested, levels.allowed, levels.effective),
            (
                Some(Autonomy::Strict),
                Autonomy::Autonomous,
                Autonomy::Strict
            )
        );
    }

    #[test]
    fn the_default_ceiling_is_standard() {
        assert_eq!(Ceilings::default().allowed("any/repo"), Autonomy::Standard);
    }
}
