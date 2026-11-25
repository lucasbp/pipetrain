use std::cmp::Ordering;
use std::collections::BTreeSet;

use super::{Builder, Rule};
use crate::detect;
use crate::feed;
use crate::model::Question;
use crate::planner::behind::{Feeds, is_pin};
use crate::publishers::{Publishers, References, dependency_pairs};

fn short(slug: &str) -> &str {
    slug.rsplit('/').next().unwrap_or(slug)
}

enum Verdict {
    Keep(String),
    Reverse(String),
}

fn published_in(feeds: &Feeds, package: &str) -> Option<Vec<String>> {
    feeds
        .get(package)
        .or_else(|| feeds.get(&package.to_lowercase()))
        .filter(|versions| !versions.is_empty())
        .cloned()
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

    fn verdict(&self, publisher: &str, consumer: &str, package: &str) -> Verdict {
        let base = format!(
            "{} references {package}, published by {}",
            short(consumer),
            short(publisher)
        );
        let evidence = &self.rules.evidence;
        let Some(published) = published_in(&evidence.feeds, package) else {
            return Verdict::Keep(format!(
                "{base} (kept by doubt: the feed of {package} could not be read)"
            ));
        };
        let pins: Vec<&String> = evidence
            .pins
            .get(&(consumer.to_string(), package.to_string()))
            .map(|set| set.iter().filter(|spec| is_pin(spec)).collect())
            .unwrap_or_default();
        if pins.is_empty() {
            return Verdict::Keep(format!("{base} (kept by doubt: version not resolved)"));
        }
        let missing = pins.iter().find(|pin| {
            !published
                .iter()
                .any(|version| feed::compare(pin, version) == Ordering::Equal)
        });
        match missing {
            Some(pin) => Verdict::Keep(format!(
                "{} references {package} {pin}, which is not published yet, so it waits for {}",
                short(consumer),
                short(publisher)
            )),
            None => Verdict::Reverse(format!(
                "{} pins {package} {}, already published; merging before {} avoids a forced bump",
                short(consumer),
                pins.iter()
                    .map(|pin| pin.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                short(publisher)
            )),
        }
    }

    fn link_repos(&mut self, publisher: &str, consumer: &str, package: &str) {
        let firsts = self.by_repo.get(publisher).cloned().unwrap_or_default();
        let seconds = self.by_repo.get(consumer).cloned().unwrap_or_default();
        let verdict = self.verdict(publisher, consumer, package);
        for &first in &firsts {
            for &second in &seconds {
                let ordered = self.graph.preds[second].iter().any(|e| e.from == first);
                if !ordered && detect::bumped_packages(&self.prs[second].files).contains(package) {
                    continue;
                }
                let verdict = if ordered {
                    &Verdict::Keep(String::new())
                } else {
                    &verdict
                };
                match verdict {
                    Verdict::Keep(reason) => self.link_package(
                        first,
                        second,
                        Rule::Dependency,
                        reason.clone(),
                        package,
                        None,
                    ),
                    Verdict::Reverse(reason) => self.link(
                        second,
                        first,
                        Rule::PackageBeforePublisher,
                        reason.clone(),
                        false,
                    ),
                }
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
