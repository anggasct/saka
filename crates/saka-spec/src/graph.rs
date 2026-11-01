// Every cycle comes back once, rotated to start at its smallest node and listed in edge order.
pub(crate) fn cycles(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        Open,
        Done,
    }
    fn visit(
        node: usize,
        edges: &[Vec<usize>],
        marks: &mut [Mark],
        path: &mut Vec<usize>,
        found: &mut Vec<Vec<usize>>,
    ) {
        marks[node] = Mark::Open;
        path.push(node);
        for &next in &edges[node] {
            match marks.get(next) {
                Some(Mark::New) => visit(next, edges, marks, path, found),
                Some(Mark::Open) => {
                    if let Some(start) = path.iter().position(|&n| n == next) {
                        let mut cycle = path[start..].to_vec();
                        if let Some(min) = (0..cycle.len()).min_by_key(|&i| cycle[i]) {
                            cycle.rotate_left(min);
                        }
                        if !found.contains(&cycle) {
                            found.push(cycle);
                        }
                    }
                }
                _ => {}
            }
        }
        path.pop();
        marks[node] = Mark::Done;
    }
    let mut marks = vec![Mark::New; edges.len()];
    let mut found = Vec::new();
    for node in 0..edges.len() {
        if marks[node] == Mark::New {
            visit(node, edges, &mut marks, &mut Vec::new(), &mut found);
        }
    }
    found
}
