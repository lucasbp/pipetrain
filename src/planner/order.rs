use super::{Builder, Rule};

impl Builder<'_> {
    pub(super) fn apply_order(&mut self) {
        let ranks: Vec<Vec<Option<usize>>> = self
            .prs
            .iter()
            .map(|pr| self.rules.order_ranks(&pr.key.slug()))
            .collect();
        let lists = ranks.first().map_or(0, Vec::len);
        for list in 0..lists {
            let column: Vec<Option<usize>> = ranks.iter().map(|row| row[list]).collect();
            for (a, rank_a) in column.iter().enumerate() {
                for (b, rank_b) in column.iter().enumerate() {
                    self.link_ranked(a, b, *rank_a, *rank_b);
                }
            }
        }
    }

    fn link_ranked(&mut self, a: usize, b: usize, rank_a: Option<usize>, rank_b: Option<usize>) {
        let (Some(first), Some(second)) = (rank_a, rank_b) else {
            return;
        };
        if first < second {
            self.link(a, b, Rule::Order, "config order".to_string(), true);
        }
    }
}
