use super::model::Commit;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

/// Order commits so children come before parents (reverse topological order).
///
/// Uses Kahn's algorithm on the reversed DAG (edges point child -> parent): a
/// commit becomes ready only once all of its children have been emitted, so
/// parents always follow children.
///
/// Among ready commits we always pick the newest by committer date, matching
/// `git log --date-order`. This interleaves branches chronologically (the way
/// the graph clusters them) rather than anchoring every branch to its fork point.
/// It also means a branch whose commits carry recent or future dates floats to
/// the top of the graph — the same behavior `git log --date-order` exhibits.
///
/// The graph loader no longer calls this: it takes its order straight from
/// `git log --date-order` so history can be paged consistently. This remains a
/// native utility for callers that need to sort an in-memory DAG themselves.
pub fn topo_sort(commits: Vec<Commit>) -> Vec<Commit> {
    let by_id: HashMap<&str, usize> = commits
        .iter()
        .enumerate()
        .map(|(index, commit)| (commit.id.as_str(), index))
        .collect();

    // `children_left[c]` = number of children of `c` that have not been emitted
    // yet. A commit can only be emitted once all of its children have been, so
    // parents always follow their children (reverse topological order).
    let mut children_left = vec![0; commits.len()];
    for c in &commits {
        for p in &c.parents {
            if let Some(&parent_index) = by_id.get(p.as_str()) {
                children_left[parent_index] += 1;
            }
        }
    }

    // Each commit becomes ready exactly once, when its last child is emitted.
    // Store indices in the heap so commit objects and their messages don't need
    // to be cloned just to build the ordered result.
    let mut heap: BinaryHeap<(i64, Reverse<String>, usize)> = BinaryHeap::new();
    for (index, commit) in commits.iter().enumerate() {
        if children_left[index] == 0 {
            heap.push((
                commit.committer_timestamp,
                Reverse(commit.id.clone()),
                index,
            ));
        }
    }

    let mut order = Vec::with_capacity(commits.len());
    while let Some((_, _, index)) = heap.pop() {
        order.push(index);
        for parent in &commits[index].parents {
            if let Some(&parent_index) = by_id.get(parent.as_str()) {
                children_left[parent_index] -= 1;
                if children_left[parent_index] == 0 {
                    let parent = &commits[parent_index];
                    heap.push((
                        parent.committer_timestamp,
                        Reverse(parent.id.clone()),
                        parent_index,
                    ));
                }
            }
        }
    }

    drop(by_id);
    let mut commits = commits.into_iter().map(Some).collect::<Vec<_>>();
    order
        .into_iter()
        .map(|index| commits[index].take().expect("commit emitted once"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, parents: &[&str], timestamp: i64) -> Commit {
        Commit {
            id: id.to_string(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            summary: format!("commit {id}"),
            message: format!("commit {id}"),
            author_name: "Test".into(),
            author_email: "test@example.com".into(),
            timestamp,
            committer_timestamp: timestamp,
            timezone_offset: 0,
            committer_timezone_offset: 0,
        }
    }

    fn sort(commits: Vec<Commit>) -> Vec<Commit> {
        topo_sort(commits)
    }

    #[test]
    fn linear_history_keeps_order() {
        let commits = vec![
            commit("c1", &[], 1),
            commit("c2", &["c1"], 2),
            commit("c3", &["c2"], 3),
        ];
        let sorted = sort(commits);
        let ids: Vec<&str> = sorted.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["c3", "c2", "c1"]);
    }

    #[test]
    fn newest_tip_comes_first() {
        // Without a head, ordering falls back to newest-first.
        let commits = vec![
            commit("base", &[], 1),
            commit("old-tip", &["base"], 2),
            commit("new-tip", &["base"], 3),
        ];
        let sorted = sort(commits);
        let ids: Vec<&str> = sorted.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.first().copied(), Some("new-tip"));
        assert_eq!(ids.last().copied(), Some("base"));
    }

    #[test]
    fn merge_parents_come_after_child() {
        let commits = vec![
            commit("base", &[], 1),
            commit("left", &["base"], 2),
            commit("right", &["base"], 3),
            commit("merge", &["left", "right"], 4),
        ];
        let sorted = sort(commits);
        let ids: Vec<&str> = sorted.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.first().copied(), Some("merge"));
        assert_eq!(ids.last().copied(), Some("base"));
        for id in &["left", "right"] {
            let pos = ids.iter().position(|x| x == id).unwrap();
            let base_pos = ids.iter().position(|x| *x == "base").unwrap();
            assert!(pos < base_pos, "{id} must come before base");
        }
    }

    #[test]
    fn shared_parent_not_duplicated() {
        let commits = vec![
            commit("base", &[], 1),
            commit("a", &["base"], 2),
            commit("b", &["base"], 3),
        ];
        let sorted = sort(commits);
        let ids: Vec<&str> = sorted.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(ids.iter().filter(|&&x| x == "base").count(), 1);
    }

    #[test]
    fn future_dated_side_branch_floats_to_top() {
        // mainline: m1 <- m2 <- m3. Side branch forked at m2 with future-dated
        // commits: s1(ts=99) <- s2(ts=100). Pure date order floats the future
        // branch above the mainline — matching `git log --date-order`.
        let commits = vec![
            commit("s2", &["s1"], 100),
            commit("s1", &["m2"], 99),
            commit("m3", &["m2"], 3),
            commit("m2", &["m1"], 2),
            commit("m1", &[], 1),
        ];
        let sorted = topo_sort(commits);
        let ids: Vec<&str> = sorted.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["s2", "s1", "m3", "m2", "m1"]);
    }

    #[test]
    fn dates_interleave_branches_not_mainline_first() {
        // Two ready side branches with a newer date than the mainline's next
        // commit: date order emits them first even though they are not on the
        // mainline's first-parent line.
        let commits = vec![
            commit("a", &["base"], 20),
            commit("b", &["base"], 30),
            commit("head", &["base"], 10),
            commit("base", &[], 1),
        ];
        let sorted = topo_sort(commits);
        let ids: Vec<&str> = sorted.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "a", "head", "base"]);
    }
}
