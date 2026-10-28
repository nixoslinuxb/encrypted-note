//! A tiny deterministic force-directed layout for the graph view.
//!
//! Repulsion pushes every pair apart, edges act as springs, and a weak pull
//! toward the centre keeps the graph compact. It's deterministic (no RNG) so it
//! can be unit-tested, and cheap enough to run in a background-ish way each time
//! the vault changes.

/// Compute 2D positions in roughly the `[-1, 1]` square for `n` nodes joined by
/// `edges`. Runs `iterations` relaxation steps.
pub fn layout(n: usize, edges: &[(usize, usize)], iterations: usize) -> Vec<(f32, f32)> {
    if n == 0 {
        return Vec::new();
    }
    // Start on a circle.
    let mut pos: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let a = (i as f32 / n as f32) * std::f32::consts::TAU;
            (a.cos(), a.sin())
        })
        .collect();

    let area = 1.0f32;
    let k = (area / n as f32).sqrt(); // ideal edge length
    let repulsion = k * k;

    for iter in 0..iterations {
        let cooling = 1.0 - (iter as f32 / iterations.max(1) as f32);
        let temp = 0.15 * cooling.max(0.05);
        let mut disp = vec![(0.0f32, 0.0f32); n];

        // Repulsive forces between all pairs.
        for i in 0..n {
            for j in (i + 1)..n {
                let dx = pos[i].0 - pos[j].0;
                let dy = pos[i].1 - pos[j].1;
                let dist2 = (dx * dx + dy * dy).max(1e-4);
                let force = repulsion / dist2;
                let dist = dist2.sqrt();
                let (ux, uy) = (dx / dist, dy / dist);
                disp[i].0 += ux * force;
                disp[i].1 += uy * force;
                disp[j].0 -= ux * force;
                disp[j].1 -= uy * force;
            }
        }

        // Spring attraction along edges.
        for &(a, b) in edges {
            if a >= n || b >= n || a == b {
                continue;
            }
            let dx = pos[a].0 - pos[b].0;
            let dy = pos[a].1 - pos[b].1;
            let dist = (dx * dx + dy * dy).sqrt().max(1e-4);
            let force = (dist * dist) / k;
            let (ux, uy) = (dx / dist, dy / dist);
            disp[a].0 -= ux * force;
            disp[a].1 -= uy * force;
            disp[b].0 += ux * force;
            disp[b].1 += uy * force;
        }

        // Apply, with a temperature that cools over time, plus centering.
        for i in 0..n {
            let (mut dx, mut dy) = disp[i];
            dx -= pos[i].0 * 0.02;
            dy -= pos[i].1 * 0.02;
            let len = (dx * dx + dy * dy).sqrt().max(1e-4);
            let step = len.min(temp);
            pos[i].0 += (dx / len) * step;
            pos[i].1 += (dy / len) * step;
        }
    }

    // Normalise into [-1, 1].
    let max_abs = pos
        .iter()
        .fold(1e-4f32, |m, p| m.max(p.0.abs()).max(p.1.abs()));
    for p in &mut pos {
        p.0 /= max_abs;
        p.1 /= max_abs;
    }
    pos
}

/// Which nodes are within `depth` hops of `focus` (inclusive), for the local
/// graph view. Returns a boolean mask of length `n`.
pub fn neighborhood(n: usize, edges: &[(usize, usize)], focus: usize, depth: usize) -> Vec<bool> {
    let mut mask = vec![false; n];
    if focus >= n {
        return mask;
    }
    // Adjacency list.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in edges {
        if a < n && b < n && a != b {
            adj[a].push(b);
            adj[b].push(a);
        }
    }
    let mut frontier = vec![focus];
    mask[focus] = true;
    for _ in 0..depth {
        let mut next = Vec::new();
        for &node in &frontier {
            for &nb in &adj[node] {
                if !mask[nb] {
                    mask[nb] = true;
                    next.push(nb);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let a = layout(4, &[(0, 1), (1, 2)], 50);
        let b = layout(4, &[(0, 1), (1, 2)], 50);
        assert_eq!(a, b);
    }

    #[test]
    fn empty_is_empty() {
        assert!(layout(0, &[], 10).is_empty());
    }

    #[test]
    fn within_unit_box() {
        let pos = layout(6, &[(0, 1), (2, 3), (4, 5)], 80);
        for (x, y) in pos {
            assert!(x.abs() <= 1.001 && y.abs() <= 1.001);
        }
    }

    #[test]
    fn neighborhood_respects_depth() {
        // Chain 0-1-2-3.
        let edges = [(0, 1), (1, 2), (2, 3)];
        let m1 = neighborhood(4, &edges, 0, 1);
        assert_eq!(m1, vec![true, true, false, false]);
        let m2 = neighborhood(4, &edges, 0, 2);
        assert_eq!(m2, vec![true, true, true, false]);
        let mall = neighborhood(4, &edges, 0, 10);
        assert_eq!(mall, vec![true, true, true, true]);
    }

    #[test]
    fn neighborhood_of_unknown_focus_is_empty() {
        let m = neighborhood(3, &[(0, 1)], 9, 2);
        assert_eq!(m, vec![false, false, false]);
    }
}
