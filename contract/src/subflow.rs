//! A flow used inside another. What a gateway runs and a reviewer simulates is the whole thing, with
//! nothing left to look up, so a used flow is copied in before a flow is checked: a step `use` becomes
//! the steps of the flow it names, wired through its `input` and `output` steps.
//!
//! Pure: `find` says what a name holds, so that the same code runs at a desk and on the platform.

use serde_json::{Map, Value, json};
use std::collections::HashSet;

/// How deep one flow can use another. Past this it is more likely a mistake than a design.
const MAX_DEPTH: usize = 4;

fn edges_of(flow: &Value) -> Vec<Value> {
    flow["edges"].as_array().cloned().unwrap_or_default()
}

fn nodes_of(flow: &Value) -> Vec<Value> {
    flow["nodes"].as_array().cloned().unwrap_or_default()
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}

/// A flow with everything it uses copied in. `find` gives a flow's body by its name, if it may be used.
pub fn expand(flow: &Value, find: &dyn Fn(&str) -> Option<Value>) -> Result<Value, String> {
    inline(flow, find, &mut HashSet::new(), 0)
}

fn inline(flow: &Value, find: &dyn Fn(&str) -> Option<Value>, above: &mut HashSet<String>, depth: usize) -> Result<Value, String> {
    let nodes = nodes_of(flow);
    if !nodes.iter().any(|n| n["kind"] == "use") {
        return Ok(flow.clone());
    }
    if depth >= MAX_DEPTH {
        return Err(format!("flows use each other more than {MAX_DEPTH} deep"));
    }
    let mut out_nodes = Vec::new();
    let mut edges = edges_of(flow);
    for node in nodes {
        if node["kind"] != "use" {
            out_nodes.push(node);
            continue;
        }
        let (id, name) = (text(&node, "id").to_string(), text(&node, "flow").to_string());
        if name.is_empty() {
            return Err(format!("{id}: say which flow it uses"));
        }
        if !above.insert(name.clone()) {
            return Err(format!("{id}: {name} ends up using itself"));
        }
        let used = find(&name).ok_or_else(|| format!("{id}: there is no approved flow called {name}"))?;
        let used = inline(&used, find, above, depth + 1)?;
        above.remove(&name);
        let prefixed = |inner: &str| format!("{id}.{inner}");
        let kind_of = |inner: &str| nodes_of(&used).iter().find(|n| n["id"] == inner).map(|n| text(n, "kind").to_string()).unwrap_or_default();
        let name_of = |inner: &str| nodes_of(&used).iter().find(|n| n["id"] == inner).map_or(String::new(), |n| text(n, "name").to_string());
        for inner in nodes_of(&used) {
            if matches!(text(&inner, "kind"), "input" | "output") {
                continue;
            }
            let mut copy = inner.clone();
            copy["id"] = json!(prefixed(text(&inner, "id")));
            out_nodes.push(copy);
        }
        let inner_edges = edges_of(&used);
        // Inside it: its wires, without the ways in and out.
        let mut wired: Vec<Value> = inner_edges
            .iter()
            .filter(|e| !matches!(kind_of(text(e, "from")).as_str(), "input") && !matches!(kind_of(text(e, "to")).as_str(), "output"))
            .map(|e| {
                let mut copy = e.clone();
                copy["from"] = json!(prefixed(text(e, "from")));
                copy["to"] = json!(prefixed(text(e, "to")));
                copy
            })
            .collect();
        // What leads to it goes to what its input leads to, and what leads from it from what reaches its output.
        let mut rest = Vec::new();
        for edge in edges.drain(..) {
            if text(&edge, "to") == id {
                let port = edge["input"].as_str().unwrap_or("in");
                let targets: Vec<&Value> = inner_edges.iter().filter(|e| kind_of(text(e, "from")) == "input" && name_of(text(e, "from")) == port).collect();
                if targets.is_empty() {
                    return Err(format!("{id}: {name} has no way in called {port}"));
                }
                for t in targets {
                    let mut copy: Map<String, Value> = edge.as_object().cloned().unwrap_or_default();
                    copy.insert("to".into(), json!(prefixed(text(t, "to"))));
                    match t.get("input") {
                        Some(input) => copy.insert("input".into(), input.clone()),
                        None => copy.remove("input"),
                    };
                    rest.push(Value::Object(copy));
                }
            } else if text(&edge, "from") == id {
                let port = edge["port"].as_str().unwrap_or("out");
                let sources: Vec<&Value> = inner_edges.iter().filter(|e| kind_of(text(e, "to")) == "output" && name_of(text(e, "to")) == port).collect();
                if sources.is_empty() {
                    return Err(format!("{id}: {name} has no way out called {port}"));
                }
                for f in sources {
                    let mut copy: Map<String, Value> = edge.as_object().cloned().unwrap_or_default();
                    copy.insert("from".into(), json!(prefixed(text(f, "from"))));
                    match f.get("port") {
                        Some(port) => copy.insert("port".into(), port.clone()),
                        None => copy.remove("port"),
                    };
                    rest.push(Value::Object(copy));
                }
            } else {
                rest.push(edge);
            }
        }
        rest.append(&mut wired);
        edges = rest;
    }
    Ok(json!({"nodes": out_nodes, "edges": edges}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cooling() -> Value {
        json!({
            "nodes": [
                {"id": "i", "kind": "input", "name": "in"},
                {"id": "high", "kind": "compare", "op": ">", "value": 80},
                {"id": "o", "kind": "output", "name": "hot"},
                {"id": "c", "kind": "output", "name": "cool"},
            ],
            "edges": [{"from": "i", "to": "high"}, {"from": "high", "port": "yes", "to": "o"}, {"from": "high", "port": "no", "to": "c"}],
        })
    }

    fn parent() -> Value {
        json!({
            "nodes": [
                {"id": "t", "kind": "metric", "metric": "temp"},
                {"id": "u", "kind": "use", "flow": "cooling"},
                {"id": "a", "kind": "alarm", "name": "Hot"},
            ],
            "edges": [
                {"from": "t", "to": "u"},
                {"from": "u", "port": "hot", "to": "a"},
                {"from": "u", "port": "cool", "to": "a", "input": "clear"},
            ],
        })
    }

    #[test]
    fn a_used_flow_is_copied_in_and_wired_through() {
        let expanded = expand(&parent(), &|name| (name == "cooling").then(cooling)).unwrap();
        let ids: Vec<&str> = expanded["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["t", "u.high", "a"], "its ways in and out go, and nothing is left to look up");
        let compiled = crate::flow::parse(&expanded.to_string()).unwrap();
        let mut state = crate::flow::State::default();
        let at = "2026-11-02T10:00:00Z".parse().unwrap();
        let hot = crate::flow::Input { device: "A".into(), metric: Some("temp".into()), value: json!(90), ..Default::default() };
        let out = compiled.input(&mut state, &hot, at);
        assert!(matches!(&out.effects[0], crate::flow::Effect::Alarm { raised: true, .. }), "{out:?}");
        let cool = crate::flow::Input { value: json!(20), ..hot };
        let out = compiled.input(&mut state, &cool, at);
        assert!(matches!(&out.effects[0], crate::flow::Effect::Alarm { raised: false, .. }), "{out:?}");
    }

    #[test]
    fn what_is_not_there_says_so() {
        assert!(expand(&parent(), &|_| None).unwrap_err().contains("no approved flow called cooling"));
        let mut wrong = parent();
        wrong["edges"][1]["port"] = json!("warm");
        assert!(expand(&wrong, &|_| Some(cooling())).unwrap_err().contains("no way out called warm"));
        wrong = parent();
        wrong["edges"][0]["input"] = json!("other");
        assert!(expand(&wrong, &|_| Some(cooling())).unwrap_err().contains("no way in called other"));
    }

    #[test]
    fn a_flow_cannot_use_itself_by_any_road() {
        let loops = json!({"nodes": [{"id": "u", "kind": "use", "flow": "a"}], "edges": []});
        assert!(expand(&loops, &|_| Some(loops.clone())).unwrap_err().contains("using itself"));
    }

    #[test]
    fn a_flow_that_uses_nothing_is_itself() {
        let plain = json!({"nodes": [{"id": "t", "kind": "metric", "metric": "m"}], "edges": []});
        assert_eq!(expand(&plain, &|_| None).unwrap(), plain);
    }
}
