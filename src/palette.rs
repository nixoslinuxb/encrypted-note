//! Fuzzy matching for the command palette and quick switcher.

/// Score a `query` against a `candidate`, returning `None` if the query is not
/// a subsequence of the candidate. Higher scores are better. Matching is
/// case-insensitive; bonuses reward consecutive runs and word boundaries.
pub fn score(query: &str, candidate: &str) -> Option<i32> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    if q.is_empty() {
        return Some(0);
    }
    let cand: Vec<char> = candidate.chars().collect();
    let lower: Vec<char> = candidate.to_lowercase().chars().collect();

    let mut score = 0i32;
    let mut qi = 0usize;
    let mut last_match: Option<usize> = None;
    for (i, ch) in lower.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if *ch == q[qi] {
            score += 10;
            if last_match == Some(i.saturating_sub(1)) {
                score += 15; // consecutive
            }
            let boundary = i == 0
                || matches!(
                    cand.get(i.wrapping_sub(1)),
                    Some(' ') | Some('_') | Some('-') | Some('/') | Some('.')
                );
            if boundary {
                score += 20; // start of a word
            }
            last_match = Some(i);
            qi += 1;
        }
    }
    if qi == q.len() {
        // Prefer shorter candidates, all else equal.
        score -= (cand.len() as i32) / 4;
        Some(score)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn subsequence_matches() {
        assert!(score("nt", "notes").is_some());
        assert!(score("ote", "notes").is_some());
        assert!(score("ns", "notes").is_some());
        assert!(score("z", "notes").is_none());
        // "est" is not a subsequence of "notes" (no 't' after 's').
        assert!(score("est", "notes").is_none());
    }

    #[test]
    fn word_boundary_scores_higher() {
        let boundary = score("d", "daily note").unwrap();
        let mid = score("d", "weekday").unwrap();
        assert!(boundary > mid);
    }
}
