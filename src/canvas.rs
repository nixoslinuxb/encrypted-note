//! Obsidian-compatible `.canvas` files.
//!
//! A canvas is a JSON document with `nodes` (text cards, file cards, groups)
//! and `edges` connecting them. We support the text/file/graph node shapes and
//! a simple undirected edge; colours and sides are preserved on round-trip.

use serde::{Deserialize, Serialize};

fn default_w() -> f32 {
    240.0
}
fn default_h() -> f32 {
    120.0
}
fn default_kind() -> String {
    "text".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Node {
    pub id: String,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    #[serde(default = "default_w")]
    pub width: f32,
    #[serde(default = "default_h")]
    pub height: f32,
    #[serde(rename = "type", default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Edge {
    pub id: String,
    #[serde(rename = "fromNode")]
    pub from_node: String,
    #[serde(rename = "toNode")]
    pub to_node: String,
    #[serde(rename = "fromSide", default)]
    pub from_side: String,
    #[serde(rename = "toSide", default)]
    pub to_side: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Canvas {
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl Canvas {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Bounding box of all nodes, as `(min_x, min_y, max_x, max_y)`.
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for n in &self.nodes {
            b.0 = b.0.min(n.x);
            b.1 = b.1.min(n.y);
            b.2 = b.2.max(n.x + n.width);
            b.3 = b.3.max(n.y + n.height);
        }
        if self.nodes.is_empty() {
            (0.0, 0.0, 0.0, 0.0)
        } else {
            b
        }
    }

    /// Create a new empty text node, returning its id.
    pub fn add_text_node(&mut self, x: f32, y: f32) -> String {
        let id = self.next_id();
        self.nodes.push(Node {
            id: id.clone(),
            x,
            y,
            width: default_w(),
            height: default_h(),
            kind: "text".to_string(),
            text: String::new(),
            file: None,
            color: None,
        });
        id
    }

    /// Create a file card pointing at a note.
    pub fn add_file_node(&mut self, file: &str, x: f32, y: f32) -> String {
        let id = self.next_id();
        self.nodes.push(Node {
            id: id.clone(),
            x,
            y,
            width: default_w(),
            height: 84.0,
            kind: "file".to_string(),
            text: String::new(),
            file: Some(file.to_string()),
            color: None,
        });
        id
    }

    /// Create a group (a labelled container).
    pub fn add_group(&mut self, x: f32, y: f32, w: f32, h: f32) -> String {
        let id = self.next_id();
        self.nodes.push(Node {
            id: id.clone(),
            x,
            y,
            width: w.max(200.0),
            height: h.max(140.0),
            kind: "group".to_string(),
            text: "Group".to_string(),
            file: None,
            color: Some("6".to_string()),
        });
        id
    }

    /// Create a card linking to an external URL.
    pub fn add_link_node(&mut self, url: &str, x: f32, y: f32) -> String {
        let id = self.next_id();
        self.nodes.push(Node {
            id: id.clone(),
            x,
            y,
            width: default_w(),
            height: 84.0,
            kind: "link".to_string(),
            text: url.to_string(),
            file: Some(url.to_string()),
            color: None,
        });
        id
    }

    /// Remove a node and every edge touching it.
    pub fn remove_node(&mut self, id: &str) {
        self.nodes.retain(|n| n.id != id);
        self.edges.retain(|e| e.from_node != id && e.to_node != id);
    }

    /// Whether an edge already joins the two nodes (either way).
    pub fn connected(&self, a: &str, b: &str) -> bool {
        self.edges
            .iter()
            .any(|e| (e.from_node == a && e.to_node == b) || (e.from_node == b && e.to_node == a))
    }

    /// Add an edge between two nodes.
    pub fn connect(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let id = self.next_id();
        self.edges.push(Edge {
            id,
            from_node: from.to_string(),
            to_node: to.to_string(),
            from_side: String::new(),
            to_side: String::new(),
        });
    }

    fn next_id(&self) -> String {
        let mut i = 0;
        loop {
            let id = format!("n{i}");
            if !self.nodes.iter().any(|n| n.id == id) && !self.edges.iter().any(|e| e.id == id) {
                return id;
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_obsidian_json() {
        let json = r#"{
            "nodes": [
                {"id":"a","type":"text","x":0,"y":0,"width":200,"height":100,"text":"hello"},
                {"id":"b","type":"file","x":300,"y":0,"width":200,"height":100,"file":"Notes/B.md"}
            ],
            "edges": [
                {"id":"e1","fromNode":"a","toNode":"b","fromSide":"right","toSide":"left"}
            ]
        }"#;
        let canvas: Canvas = serde_json::from_str(json).unwrap();
        assert_eq!(canvas.nodes.len(), 2);
        assert_eq!(
            canvas.node("b").unwrap().file.as_deref(),
            Some("Notes/B.md")
        );
        assert_eq!(canvas.edges[0].from_node, "a");
        // Re-serialize and parse again.
        let again: Canvas = serde_json::from_str(&serde_json::to_string(&canvas).unwrap()).unwrap();
        assert_eq!(canvas, again);
    }

    #[test]
    fn defaults_fill_missing_fields() {
        let canvas: Canvas = serde_json::from_str(r#"{"nodes":[{"id":"a"}]}"#).unwrap();
        let n = canvas.node("a").unwrap();
        assert_eq!(n.kind, "text");
        assert_eq!(n.width, 240.0);
        assert!(canvas.edges.is_empty());
    }

    #[test]
    fn add_node_and_connect() {
        let mut c = Canvas::default();
        let a = c.add_text_node(0.0, 0.0);
        let b = c.add_text_node(10.0, 10.0);
        assert_ne!(a, b);
        c.connect(&a, &b);
        c.connect(&a, &a); // self-edges ignored
        assert_eq!(c.edges.len(), 1);
    }

    #[test]
    fn bounds_cover_all_nodes() {
        let mut c = Canvas::default();
        c.add_text_node(0.0, 0.0);
        c.add_text_node(100.0, 50.0);
        let (minx, miny, maxx, maxy) = c.bounds();
        assert_eq!((minx, miny), (0.0, 0.0));
        assert!(maxx >= 340.0 && maxy >= 170.0);
    }
}
