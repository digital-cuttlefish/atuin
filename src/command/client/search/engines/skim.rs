use std::path::Path;

use async_trait::async_trait;
use atuin_client::{
    database::Database, history::History, result::HistoryResult, settings::FilterMode,
};
use chrono::Utc;
use eyre::Result;
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use tokio::task::yield_now;

use super::{SearchEngine, SearchState};

pub struct Search {
    all_history: Vec<HistoryResult>,
    engine: SkimMatcherV2,
}

impl Search {
    pub fn new() -> Self {
        Search {
            all_history: vec![],
            engine: SkimMatcherV2::default(),
        }
    }
}

#[async_trait]
impl SearchEngine for Search {
    async fn full_query(
        &mut self,
        state: &SearchState,
        db: &mut dyn Database,
    ) -> Result<Vec<HistoryResult>> {
        if self.all_history.is_empty() {
            self.all_history = db.all_with_count().await.unwrap();
        }

        Ok(fuzzy_search(&self.engine, state, &self.all_history).await)
    }
}

async fn fuzzy_search(
    engine: &SkimMatcherV2,
    state: &SearchState,
    all_history: &[HistoryResult],
) -> Vec<HistoryResult> {
    let mut set = Vec::with_capacity(200);
    let mut ranks = Vec::with_capacity(200);
    let query = state.input.as_str();
    let now = Utc::now();

    for (i, res) in all_history.iter().enumerate() {
        let history = &res.history;
        let count = &res.count;

        if i % 256 == 0 {
            yield_now().await;
        }
        match state.filter_mode {
            FilterMode::Global => {}
            FilterMode::Host if history.hostname == state.context.hostname => {}
            FilterMode::Session if history.session == state.context.session => {}
            FilterMode::Directory if history.cwd == state.context.cwd => {}
            _ => continue,
        }
        #[allow(clippy::cast_lossless, clippy::cast_precision_loss)]
        if let Some((score, indices)) = engine.fuzzy_indices(&history.command, query) {
            let begin = indices.first().copied().unwrap_or_default();

            let mut duration = ((now - history.timestamp).num_seconds() as f64).log2();
            if !duration.is_finite() || duration <= 1.0 {
                duration = 1.0;
            }
            // these + X.0 just make the log result a bit smoother.
            // log is very spiky towards 1-4, but I want a gradual decay.
            // eg:
            // log2(4) = 2, log2(5) = 2.3 (16% increase)
            // log2(8) = 3, log2(9) = 3.16 (5% increase)
            // log2(16) = 4, log2(17) = 4.08 (2% increase)
            let count = (*count as f64 + 8.0).log2();
            let begin = (begin as f64 + 16.0).log2();
            let path = path_dist(history.cwd.as_ref(), state.context.cwd.as_ref());
            let path = (path as f64 + 8.0).log2();

            // reduce longer durations, raise higher counts, raise matches close to the start
            let score = (-score as f64) * count / path / duration / begin;

            'insert: {
                // algorithm:
                // 1. find either the position that this command ranks
                // 2. find the same command positioned better than our rank.
                for i in 0..set.len() {
                    // do we out score the corrent position?
                    if ranks[i] > score {
                        ranks.insert(i, score);
                        set.insert(i, res.clone());
                        let mut j = i + 1;
                        while j < set.len() {
                            // remove duplicates that have a worse score
                            if set[j].history.command == history.command {
                                ranks.remove(j);
                                set.remove(j);

                                // break this while loop because there won't be any other
                                // duplicates.
                                break;
                            }
                            j += 1;
                        }

                        // keep it limited
                        if ranks.len() > 200 {
                            ranks.pop();
                            set.pop();
                        }

                        break 'insert;
                    }
                    // don't continue if this command has a better score already
                    if set[i].history.command == history.command {
                        break 'insert;
                    }
                }

                if set.len() < 200 {
                    ranks.push(score);
                    set.push(res.clone());
                }
            }
        }
    }

    set
}

fn path_dist(a: &Path, b: &Path) -> usize {
    let mut a: Vec<_> = a.components().collect();
    let b: Vec<_> = b.components().collect();

    let mut dist = 0;

    // pop a until there's a common anscestor
    while !b.starts_with(&a) {
        dist += 1;
        a.pop();
    }

    b.len() - a.len() + dist
}
