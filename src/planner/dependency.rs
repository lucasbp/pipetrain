use std::collections::{BTreeMap, BTreeSet};

use super::{Builder, Rule};
use crate::model::Question;
use crate::publishers::{Publishers, References};

type Pairs = BTreeMap<(String, String), String>;

fn short(slug: &str) -> &str {
    slug.rsplit('/').next().unwrap_or(slug)
}

impl Builder<'_> {
    pub(super) fn apply_dependency(&mut self, references: &References, scanned: &Publishers) {
        let mut publishers = scanned.clone();
        for (package, slug) in &self.rules.packages {
            publishers.insert(package.clone(), BTreeSet::from([slug.clone()]));
        }
        let pairs = self.dependency_pairs(references, &publishers);
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

    fn dependency_pairs(&self, references: &References, publishers: &Publishers) -> Pairs {
        let mut pairs = Pairs::new();
        for (consumer, packages) in references {
            if !self.by_repo.contains_key(consumer) {
                continue;
            }
            for package in packages {
                let Some(repos) = publishers.get(package) else {
                    continue;
                };
                if repos.contains(consumer) {
                    continue;
                }
                for publisher in repos.iter().filter(|r| self.by_repo.contains_key(*r)) {
                    pairs
                        .entry((publisher.clone(), consumer.clone()))
                        .or_insert_with(|| package.clone());
                }
            }
        }
        pairs
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
                self.link(first, second, Rule::Dependency, reason.clone(), true);
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
