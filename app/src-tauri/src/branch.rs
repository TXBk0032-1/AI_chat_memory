use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::models::{BranchNode, BranchOverview};

#[derive(Default)]
struct RawNode {
    parent: String,
    children: Vec<String>,
}

pub fn build_overview(mut nodes: Vec<BranchNode>, raw_data: &Value) -> BranchOverview {
    nodes.sort_by_key(|node| node.seq);
    let visible = nodes
        .iter()
        .map(|node| (node.node_id.clone(), node.seq))
        .collect::<HashMap<_, _>>();
    let raw_nodes = raw_nodes(raw_data);

    let mut parents = HashMap::new();
    for node in &nodes {
        let parent = nearest_visible_parent(node, &visible, &raw_nodes);
        parents.insert(node.node_id.clone(), parent);
    }
    remove_parent_cycles(&nodes, &mut parents);

    let mut children = HashMap::<String, Vec<String>>::new();
    for node in &nodes {
        if let Some(parent) = parents
            .get(&node.node_id)
            .filter(|parent| !parent.is_empty())
        {
            children
                .entry(parent.clone())
                .or_default()
                .push(node.node_id.clone());
        }
    }

    for (parent, child_ids) in &mut children {
        let raw_order = visible_descendants(parent, &visible, &raw_nodes);
        child_ids.sort_by_key(|child| {
            (
                raw_order
                    .iter()
                    .position(|candidate| candidate == child)
                    .unwrap_or(usize::MAX),
                visible.get(child).copied().unwrap_or(i64::MAX),
            )
        });
        child_ids.dedup();
    }

    for node in &mut nodes {
        node.parent_node_id = parents.remove(&node.node_id).unwrap_or_default();
        node.children_node_ids = children.remove(&node.node_id).unwrap_or_default();
    }

    let mut roots = nodes
        .iter()
        .filter(|node| node.parent_node_id.is_empty())
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    order_roots(&mut roots, &visible, &raw_nodes);
    let default_leaf_node_id = default_leaf(&nodes, &roots);
    BranchOverview {
        nodes,
        default_leaf_node_id,
    }
}

fn raw_nodes(raw_data: &Value) -> HashMap<String, RawNode> {
    raw_data
        .get("mapping")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(key, value)| {
            let id = value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .to_owned();
            let parent = value
                .get("parent")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let children = value
                .get("children")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            (id, RawNode { parent, children })
        })
        .collect()
}

fn nearest_visible_parent(
    node: &BranchNode,
    visible: &HashMap<String, i64>,
    raw_nodes: &HashMap<String, RawNode>,
) -> String {
    let mut current = raw_nodes
        .get(&node.node_id)
        .map(|raw| raw.parent.as_str())
        .filter(|parent| !parent.is_empty())
        .unwrap_or(&node.parent_node_id)
        .to_owned();
    let mut visited = HashSet::from([node.node_id.clone()]);
    while !current.is_empty() && visited.insert(current.clone()) {
        if visible.contains_key(&current) {
            return current;
        }
        current = raw_nodes
            .get(&current)
            .map(|raw| raw.parent.clone())
            .unwrap_or_default();
    }
    String::new()
}

fn visible_descendants(
    parent: &str,
    visible: &HashMap<String, i64>,
    raw_nodes: &HashMap<String, RawNode>,
) -> Vec<String> {
    fn visit(
        id: &str,
        visible: &HashMap<String, i64>,
        raw_nodes: &HashMap<String, RawNode>,
        visited: &mut HashSet<String>,
        output: &mut Vec<String>,
    ) {
        if !visited.insert(id.to_owned()) {
            return;
        }
        if visible.contains_key(id) {
            output.push(id.to_owned());
            return;
        }
        if let Some(node) = raw_nodes.get(id) {
            for child in &node.children {
                visit(child, visible, raw_nodes, visited, output);
            }
        }
    }

    let mut output = Vec::new();
    let mut visited = HashSet::from([parent.to_owned()]);
    if let Some(node) = raw_nodes.get(parent) {
        for child in &node.children {
            visit(child, visible, raw_nodes, &mut visited, &mut output);
        }
    }
    output
}

fn remove_parent_cycles(nodes: &[BranchNode], parents: &mut HashMap<String, String>) {
    for node in nodes {
        let mut current = node.node_id.clone();
        let mut visited = HashSet::new();
        while !current.is_empty() {
            if !visited.insert(current.clone()) {
                parents.insert(node.node_id.clone(), String::new());
                break;
            }
            current = parents.get(&current).cloned().unwrap_or_default();
        }
    }
}

fn order_roots(
    roots: &mut [String],
    visible: &HashMap<String, i64>,
    raw_nodes: &HashMap<String, RawNode>,
) {
    let mut raw_order = Vec::new();
    let mut raw_roots = raw_nodes
        .iter()
        .filter(|(_, node)| node.parent.is_empty())
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    raw_roots.sort_by_key(|id| {
        let first_visible_seq = visible.get(id).copied().or_else(|| {
            visible_descendants(id, visible, raw_nodes)
                .iter()
                .filter_map(|descendant| visible.get(descendant))
                .copied()
                .min()
        });
        (first_visible_seq.unwrap_or(i64::MAX), id.clone())
    });
    for root in raw_roots {
        if visible.contains_key(&root) {
            raw_order.push(root.clone());
        }
        raw_order.extend(visible_descendants(&root, visible, raw_nodes));
    }
    roots.sort_by_key(|root| {
        (
            raw_order
                .iter()
                .position(|candidate| candidate == root)
                .unwrap_or(usize::MAX),
            visible.get(root).copied().unwrap_or(i64::MAX),
        )
    });
}

fn default_leaf(nodes: &[BranchNode], roots: &[String]) -> String {
    let by_id = nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let mut current = roots.last().cloned().unwrap_or_default();
    let mut visited = HashSet::new();
    while !current.is_empty() && visited.insert(current.clone()) {
        let Some(child) = by_id
            .get(current.as_str())
            .and_then(|node| node.children_node_ids.last())
        else {
            break;
        };
        current.clone_from(child);
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, parent: &str, seq: i64) -> BranchNode {
        BranchNode {
            message_id: format!("message-{id}"),
            seq,
            role: "assistant".into(),
            node_id: id.into(),
            parent_node_id: parent.into(),
            children_node_ids: Vec::new(),
            preview: id.into(),
        }
    }

    #[test]
    fn collapses_placeholders_and_preserves_child_order() {
        let raw = json!({"mapping": {
            "root": {"id":"root", "parent":null, "children":["a", "placeholder"]},
            "a": {"id":"a", "parent":"root", "children":[]},
            "placeholder": {"id":"placeholder", "parent":"root", "children":["c", "b"]},
            "b": {"id":"b", "parent":"placeholder", "children":[]},
            "c": {"id":"c", "parent":"placeholder", "children":[]}
        }});
        let overview = build_overview(
            vec![
                node("a", "root", 0),
                node("b", "placeholder", 1),
                node("c", "placeholder", 2),
            ],
            &raw,
        );
        assert_eq!(
            overview
                .nodes
                .iter()
                .map(|node| node.parent_node_id.as_str())
                .collect::<Vec<_>>(),
            ["", "", ""]
        );
        assert_eq!(overview.default_leaf_node_id, "b");
    }

    #[test]
    fn chooses_the_last_root_in_raw_tree_order() {
        let raw = json!({"mapping": {
            "root": {"id":"root", "parent":null, "children":["late", "early"]},
            "early": {"id":"early", "parent":"root", "children":[]},
            "late": {"id":"late", "parent":"root", "children":[]}
        }});
        let overview = build_overview(
            vec![node("early", "root", 0), node("late", "root", 1)],
            &raw,
        );
        assert_eq!(overview.default_leaf_node_id, "early");
    }

    #[test]
    fn connects_visible_descendants_through_placeholder() {
        let raw = json!({"mapping": {
            "a": {"id":"a", "parent":null, "children":["placeholder"]},
            "placeholder": {"id":"placeholder", "parent":"a", "children":["c", "b"]},
            "b": {"id":"b", "parent":"placeholder", "children":[]},
            "c": {"id":"c", "parent":"placeholder", "children":[]}
        }});
        let overview = build_overview(
            vec![
                node("a", "", 0),
                node("b", "placeholder", 1),
                node("c", "placeholder", 2),
            ],
            &raw,
        );
        assert_eq!(overview.nodes[0].children_node_ids, ["c", "b"]);
        assert_eq!(overview.nodes[1].parent_node_id, "a");
        assert_eq!(overview.nodes[2].parent_node_id, "a");
        assert_eq!(overview.default_leaf_node_id, "b");
    }

    #[test]
    fn handles_orphans_and_cycles_with_sequence_fallback() {
        let raw = json!({"mapping": {
            "a": {"id":"a", "parent":"b", "children":["b"]},
            "b": {"id":"b", "parent":"a", "children":["a"]}
        }});
        let overview = build_overview(
            vec![
                node("a", "b", 1),
                node("b", "a", 2),
                node("orphan", "missing", 3),
            ],
            &raw,
        );
        assert_eq!(overview.nodes.len(), 3);
        assert!(
            overview
                .nodes
                .iter()
                .any(|node| node.node_id == "a" && node.parent_node_id.is_empty())
        );
        assert_eq!(overview.default_leaf_node_id, "orphan");
    }
}
