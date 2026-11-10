//! Wire representation of native shot data. Preserve shared/cyclic structs and
//! bound methods; JSON alone loses the callbacks used by projectile modifiers.
use crate::engine::{G, V, real, truthy};
use crate::gm::*;
use bitcode::{Decode, Encode};
use std::collections::HashMap;

#[derive(Clone, Debug, Encode, Decode)]
pub enum Value {
    Undefined,
    Number(f64),
    Bool(bool),
    Text(String),
    Asset(u64),
    Node(u16),
}
#[derive(Clone, Debug, Encode, Decode)]
pub enum Node {
    Struct(Vec<(String, Value)>),
    Array(Vec<Value>),
    Method { script: u64, scope: Value },
}
#[derive(Clone, Debug, Encode, Decode)]
pub struct Graph {
    pub root: Value,
    pub nodes: Vec<Node>,
}
impl Graph {
    pub fn sane(&self) -> bool {
        fn value(v: &Value, n: usize) -> bool {
            match v {
                Value::Number(x) => x.is_finite(),
                Value::Text(s) => s.len() < 8192,
                Value::Node(i) => (*i as usize) < n,
                _ => true,
            }
        }
        self.nodes.len() <= 2048
            && value(&self.root, self.nodes.len())
            && self.nodes.iter().all(|node| match node {
                Node::Struct(fields) => fields.len() <= 256 && fields.iter().all(|(k, v)| k.len() <= 128 && value(v, self.nodes.len())),
                Node::Array(a) => a.len() <= 1024 && a.iter().all(|v| value(v, self.nodes.len())),
                Node::Method { scope, .. } => value(scope, self.nodes.len()),
            })
    }
    pub fn capture(g: G, root: V) -> Option<Self> {
        let mut nodes = Vec::new();
        let mut seen = HashMap::new();
        fn visit(g: G, v: V, nodes: &mut Vec<Node>, seen: &mut HashMap<(u32, u64), u16>, depth: usize) -> Option<Value> {
            if depth > 64 || nodes.len() >= 2048 {
                return None;
            }
            Some(match v.kind() {
                KIND_UNDEFINED => Value::Undefined,
                KIND_BOOL => Value::Bool(truthy(&v)),
                KIND_REAL | KIND_INT32 | KIND_INT64 => Value::Number(v.as_f64()?),
                KIND_STRING => Value::Text(v.as_str()?),
                KIND_REF => {
                    // Shot definitions contain assets, never transport process-local instances.
                    let label = g.str_of(&v);
                    if label.starts_with("ref instance") || label.starts_with("ref tilemap") {
                        Value::Number(-4.0)
                    } else {
                        Value::Asset(v.v)
                    }
                }
                KIND_OBJECT | KIND_ARRAY => {
                    if let Some(i) = seen.get(&(v.kind(), v.v)) {
                        return Some(Value::Node(*i));
                    }
                    let i = nodes.len() as u16;
                    seen.insert((v.kind(), v.v), i);
                    nodes.push(Node::Struct(Vec::new()));
                    let node = if v.kind() == KIND_ARRAY {
                        let n = g.arr_len(v);
                        if n > 1024 {
                            return None;
                        }
                        Node::Array(
                            (0..n)
                                .map(|j| visit(g, g.arr_get(v, j), nodes, seen, depth + 1))
                                .collect::<Option<_>>()?,
                        )
                    } else if truthy(&g.call("is_method", &[v])) {
                        let script = g.call("method_get_index", &[v]);
                        Node::Method {
                            script: script.v,
                            scope: visit(g, g.call("method_get_self", &[v]), nodes, seen, depth + 1)?,
                        }
                    } else {
                        let names = g.call("variable_struct_get_names", &[v]);
                        let n = g.arr_len(names);
                        if n > 256 {
                            return None;
                        }
                        let mut fields = Vec::new();
                        for j in 0..n {
                            let k = g.str_of(&g.arr_get(names, j));
                            // UI metadata is never consumed by the shot factory or its descendants.
                            if matches!(k.as_str(), "infotxt" | "unlock_txt" | "draw_info") {
                                continue;
                            }
                            fields.push((k.clone(), visit(g, g.get(v, &k), nodes, seen, depth + 1)?));
                        }
                        Node::Struct(fields)
                    };
                    nodes[i as usize] = node;
                    Value::Node(i)
                }
                _ => return None,
            })
        }
        let root = visit(g, root, &mut nodes, &mut seen, 0)?;
        Some(Self { root, nodes })
    }
    pub fn restore_visual(&self, g: G) -> Option<V> {
        if !self.sane() {
            return None;
        }
        // Keep all intermediate nodes alive while native allocation can run GC.
        // Rust's vector is not a GameMaker GC root.
        let roots=g.call("array_create",&[real(0.0)]);
        g.call("array_push",&[g.global("focmp_shot_roots"),roots]);
        let mut values: Vec<V> = self
            .nodes
            .iter()
            .map(|n| {let value=match n {
                Node::Array(a) => g.call("array_create", &[real(a.len() as f64)]),
                Node::Struct(_) => g.call("json_parse", &[g.s("{}")]),
                Node::Method { .. } => V::undefined(),
            };g.call("array_push",&[roots,value]);value})
            .collect();
        fn get(v: &Value, values: &[V], g: G) -> V {
            match v {
                Value::Undefined => V::undefined(),
                Value::Number(x) => real(*x),
                Value::Bool(x) => V::boolean(*x),
                Value::Text(s) => g.s(s),
                Value::Asset(bits) => V {
                    v: *bits,
                    flags: 0,
                    kind: KIND_REF,
                },
                Value::Node(i) => values[*i as usize],
            }
        }
        for (i, n) in self.nodes.iter().enumerate() {
            if let Node::Method { script, scope } = n {
                values[i] = g.call(
                    "method",
                    &[
                        get(scope, &values, g),
                        V {
                            v: *script,
                            flags: 0,
                            kind: KIND_REF,
                        },
                    ],
                );
                g.call("array_push",&[roots,values[i]]);
            }
        }
        for (i, n) in self.nodes.iter().enumerate() {
            match n {
                Node::Struct(fields) => {
                    for (k, v) in fields {
                        g.set(values[i], k, get(v, &values, g));
                    }
                    // Each receiver owns damage to its local player. Keep the
                    // native damage/collision/modifier rules, but tag every nested
                    // definition so replay cannot damage enemies a second time.
                    g.set(values[i], "focmp_visual", V::boolean(true));
                }
                Node::Array(a) => {
                    for (j, v) in a.iter().enumerate() {
                        g.call("array_set", &[values[i], real(j as f64), get(v, &values, g)]);
                    }
                }
                Node::Method { .. } => {}
            }
        }
        Some(get(&self.root, &values, g))
    }
}
