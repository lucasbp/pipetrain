use std::collections::BTreeSet;

use super::{Builder, Rule};
use crate::model::Question;
use crate::publishers::{Publishers, References, dependency_pairs};

fn short(slug: &str) -> &str {
    slug.rsplit('/').next().unwrap_or(slug)
}

impl Builder<'_> {
    pub(super) fn apply_dependency(&mut self, references: &References, scanned: &Publishers) {
        let mut publishers = scanned.clone();
        for (package, slug) in &self.rules.packages {
            publishers.insert(package.clone(), BTreeSet::from([slug.clone()]));
        }
        let pairs = dependency_pairs(
            |repo| self.by_repo.contains_key(repo),
            references,
            &publishers,
        );
        for ((publisher, consumer), package) in &pairs {
            let back = pairs.get(&(consumer.clone(), publisher.clone()));
            match back {
                Some(other) if publisher < consumer => {
                    self.ask_cycle(publisher, consumer, package, other)
                }
                Some(_) => {}
                None => self.link_repos(publisher, consumer, package),
            }
        }
    }

    fn link_repos(&mut self, publisher: &str, consumer: &str, package: &str) {
        let firsts = self.by_repo.get(publisher).cloned().unwrap_or_default();
        let seconds = self.by_repo.get(consumer).cloned().unwrap_or_default();
        let reason = format!(
            "{} references {package}, published by {}",
            short(consumer),
            short(publisher)
        );
        for &first in &firsts {
            for &second in &seconds {
                self.link_package(
                    first,
                    second,
                    Rule::Dependency,
                    reason.clone(),
                    package,
                    None,
                );
            }
        }
    }

    fn ask_cycle(&mut self, a: &str, b: &str, package_a: &str, package_b: &str) {
        self.questions.push(Question::new(
            None,
            "dependency-cycle",
            format!(
                "{a} and {b} reference each other's packages ({b} uses {package_a}, {a} uses {package_b}), so no order between them was derived"
            ),
            "pin the order under [[order]] in the config",
        ));
    }
}
