//! glTF 1.0, converted to the 2.0 JSON the rest of this crate reads: 1.0 keeps everything
//! in objects keyed by id where 2.0 has arrays, a node may hold several meshes, and a
//! stride belongs to the accessor. Materials become their diffuse colour or texture;
//! shaders and techniques aren't run.

use serde_json::{Map, Value, json};

/// Whether this JSON is glTF 1.0: by its version, or its objects keyed by id.
pub fn is_v1(root: &Value) -> bool {
    let version = root.get("asset").and_then(|a| a.get("version"));
    match version.and_then(Value::as_str) {
        Some(v) => v.starts_with('1'),
        // 1.0 allowed the version to be a number, or missing.
        None => version.is_some_and(Value::is_number) || ["meshes", "accessors", "buffers", "nodes"].iter().any(|k| root.get(*k).is_some_and(Value::is_object)),
    }
}

/// The ids of a top-level object, in order, with their indices.
fn index<'a>(root: &'a Value, key: &str) -> (Vec<(&'a String, &'a Value)>, Map<String, Value>) {
    let items: Vec<_> = root.get(key).and_then(Value::as_object).map(|o| o.iter().collect()).unwrap_or_default();
    let ids = items.iter().enumerate().map(|(i, (k, _))| ((*k).clone(), json!(i))).collect();
    (items, ids)
}

fn id(ids: &Map<String, Value>, v: Option<&Value>) -> Option<Value> {
    ids.get(v?.as_str()?).cloned()
}

/// `root`, as glTF 2.0.
pub fn upgrade(root: &Value) -> Value {
    let (mut buffers, mut buffer_ids) = index(root, "buffers");
    // A binary glTF's body is the first buffer, the one without a URI.
    buffers.sort_by_key(|(k, _)| k.as_str() != "binary_glTF");
    for (i, (k, _)) in buffers.iter().enumerate() {
        buffer_ids.insert((*k).clone(), json!(i));
    }
    let buffers_out: Vec<Value> = buffers
        .iter()
        .map(|(k, b)| {
            let mut out = json!({"byteLength": b.get("byteLength").cloned().unwrap_or(json!(0))});
            if k.as_str() != "binary_glTF"
                && let Some(uri) = b.get("uri")
            {
                out["uri"] = uri.clone();
            }
            out
        })
        .collect();

    let (views, view_ids) = index(root, "bufferViews");
    let view = |v: &Value| {
        json!({
            "buffer": id(&buffer_ids, v.get("buffer")).unwrap_or(json!(0)),
            "byteOffset": v.get("byteOffset").cloned().unwrap_or(json!(0)),
            "byteLength": v.get("byteLength").cloned().unwrap_or(json!(0)),
        })
    };
    let mut views_out: Vec<Value> = views.iter().map(|(_, v)| view(v)).collect();

    // A stride goes on a view of its own.
    let (accessors, accessor_ids) = index(root, "accessors");
    let accessors_out: Vec<Value> = accessors
        .iter()
        .map(|(_, a)| {
            let mut out = json!({
                "byteOffset": a.get("byteOffset").cloned().unwrap_or(json!(0)),
                "componentType": a.get("componentType").cloned().unwrap_or(json!(5126)),
                "count": a.get("count").cloned().unwrap_or(json!(0)),
                "type": a.get("type").cloned().unwrap_or(json!("SCALAR")),
            });
            for k in ["min", "max"] {
                if let Some(v) = a.get(k) {
                    out[k] = v.clone();
                }
            }
            let stride = a.get("byteStride").and_then(Value::as_u64).unwrap_or(0);
            if let Some(v) = id(&view_ids, a.get("bufferView")) {
                out["bufferView"] = if stride == 0 {
                    v
                } else {
                    let mut with_stride = v.as_u64().and_then(|i| views_out.get(i as usize)).cloned().unwrap_or(json!({}));
                    with_stride["byteStride"] = json!(stride);
                    views_out.push(with_stride);
                    json!(views_out.len() - 1)
                };
            }
            out
        })
        .collect();

    let (images, image_ids) = index(root, "images");
    let images_out: Vec<Value> = images
        .iter()
        .map(|(_, i)| {
            let binary = i.get("extensions").and_then(|e| e.get("KHR_binary_glTF"));
            let mut out = json!({});
            match binary.and_then(|b| id(&view_ids, b.get("bufferView"))) {
                Some(v) => {
                    out["bufferView"] = v;
                    if let Some(m) = binary.and_then(|b| b.get("mimeType")) {
                        out["mimeType"] = m.clone();
                    }
                }
                None => {
                    if let Some(u) = i.get("uri") {
                        out["uri"] = u.clone();
                    }
                }
            }
            out
        })
        .collect();
    let (textures, texture_ids) = index(root, "textures");
    let textures_out: Vec<Value> = textures.iter().map(|(_, t)| id(&image_ids, t.get("source")).map_or(json!({}), |s| json!({"source": s}))).collect();

    let (materials, material_ids) = index(root, "materials");
    let materials_out: Vec<Value> = materials
        .iter()
        .map(|(_, m)| {
            // The colour is on the material, or a default of its technique.
            let technique = m.get("technique").and_then(Value::as_str).and_then(|t| root.get("techniques")?.get(t));
            let default = technique.and_then(|t| t.get("parameters")?.get("diffuse")?.get("value"));
            let diffuse = m.get("values").and_then(|v| v.get("diffuse")).or(default);
            let mut pbr = json!({"metallicFactor": 0.0, "roughnessFactor": 0.8});
            match diffuse {
                Some(Value::Array(c)) if c.len() >= 3 => {
                    pbr["baseColorFactor"] = json!([c[0], c[1], c[2], c.get(3).cloned().unwrap_or(json!(1.0))]);
                }
                Some(Value::Array(c)) if c.len() == 1 => {
                    if let Some(t) = id(&texture_ids, c.first()) {
                        pbr["baseColorTexture"] = json!({"index": t});
                    }
                }
                Some(t @ Value::String(_)) => {
                    if let Some(t) = id(&texture_ids, Some(t)) {
                        pbr["baseColorTexture"] = json!({"index": t});
                    }
                }
                _ => {}
            }
            json!({"pbrMetallicRoughness": pbr})
        })
        .collect();

    let (meshes, mesh_ids) = index(root, "meshes");
    let meshes_out: Vec<Value> = meshes
        .iter()
        .map(|(_, m)| {
            let primitives: Vec<Value> = m
                .get("primitives")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .map(|p| {
                            let mut attributes = Map::new();
                            for (name, a) in p.get("attributes").and_then(Value::as_object).into_iter().flatten() {
                                // 1.0 allows `COLOR` and `TEXCOORD` without a set number.
                                let name = match name.as_str() {
                                    "COLOR" => "COLOR_0",
                                    "TEXCOORD" => "TEXCOORD_0",
                                    n => n,
                                };
                                if let Some(i) = id(&accessor_ids, Some(a)) {
                                    attributes.insert(name.to_string(), i);
                                }
                            }
                            let mut out = json!({"attributes": attributes, "mode": p.get("mode").cloned().unwrap_or(json!(4))});
                            if let Some(i) = id(&accessor_ids, p.get("indices")) {
                                out["indices"] = i;
                            }
                            if let Some(m) = id(&material_ids, p.get("material")) {
                                out["material"] = m;
                            }
                            out
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({"primitives": primitives})
        })
        .collect();

    let (nodes, node_ids) = index(root, "nodes");
    let mut nodes_out: Vec<Value> = Vec::with_capacity(nodes.len());
    let mut extra = Vec::new();
    for (_, n) in &nodes {
        let mut out = json!({});
        for k in ["matrix", "translation", "rotation", "scale"] {
            if let Some(v) = n.get(k) {
                out[k] = v.clone();
            }
        }
        let mut children: Vec<Value> = n.get("children").and_then(Value::as_array).into_iter().flatten().filter_map(|c| id(&node_ids, Some(c))).collect();
        let mut held = n.get("meshes").and_then(Value::as_array).into_iter().flatten().filter_map(|m| id(&mesh_ids, Some(m)));
        if let Some(first) = held.next() {
            out["mesh"] = first;
        }
        // A 2.0 node holds one mesh: the rest are children.
        for m in held {
            children.push(json!(nodes.len() + extra.len()));
            extra.push(json!({"mesh": m}));
        }
        out["children"] = Value::Array(children);
        nodes_out.push(out);
    }
    nodes_out.extend(extra);

    let (scenes, scene_ids) = index(root, "scenes");
    let scenes_out: Vec<Value> = scenes
        .iter()
        .map(|(_, s)| json!({"nodes": s.get("nodes").and_then(Value::as_array).into_iter().flatten().filter_map(|n| id(&node_ids, Some(n))).collect::<Vec<_>>()}))
        .collect();

    let mut out = json!({
        "asset": {"version": "2.0"},
        "buffers": buffers_out, "bufferViews": views_out, "accessors": accessors_out,
        "images": images_out, "textures": textures_out, "materials": materials_out,
        "meshes": meshes_out, "nodes": nodes_out, "scenes": scenes_out,
    });
    if let Some(s) = id(&scene_ids, root.get("scene")) {
        out["scene"] = s;
    } else if !scenes.is_empty() {
        out["scene"] = json!(0);
    }
    out
}
