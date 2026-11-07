use std::collections::HashMap;

use crate::Value;

/// What a spec describes (Sdf's spec types, same numbering as the crate format).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecType {
    Unknown,
    Attribute,
    Connection,
    Expression,
    Mapper,
    MapperArg,
    Prim,
    PseudoRoot,
    Relationship,
    RelationshipTarget,
    Variant,
    VariantSet,
}

impl SpecType {
    pub(crate) fn from_u32(v: u32) -> Self {
        match v {
            1 => SpecType::Attribute,
            2 => SpecType::Connection,
            3 => SpecType::Expression,
            4 => SpecType::Mapper,
            5 => SpecType::MapperArg,
            6 => SpecType::Prim,
            7 => SpecType::PseudoRoot,
            8 => SpecType::Relationship,
            9 => SpecType::RelationshipTarget,
            10 => SpecType::Variant,
            11 => SpecType::VariantSet,
            _ => SpecType::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub spec_type: SpecType,
    /// Field name → value, in file order (`typeName`, `default`, `primChildren`,
    /// `connectionPaths`, `interpolation`, …).
    pub fields: Vec<(String, Value)>,
}

impl Spec {
    pub fn field(&self, name: &str) -> Option<&Value> {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }
}

/// One USD layer as data, the way Sdf models it: specs by path. Prim paths look like
/// `/World/Mesh`, property paths like `/World/Mesh.points`; `/` holds the layer's
/// metadata. Nothing is composed: references, payloads, sublayers and variants are
/// left as the fields that describe them.
#[derive(Debug, Clone, Default)]
pub struct Layer {
    pub specs: HashMap<String, Spec>,
    /// For a text layer, the line (from 1) each prim spec starts on, by path; empty for
    /// a binary one.
    pub lines: HashMap<String, u32>,
}

impl Layer {
    pub fn spec(&self, path: &str) -> Option<&Spec> {
        self.specs.get(path)
    }

    pub fn field(&self, path: &str, name: &str) -> Option<&Value> {
        self.spec(path)?.field(name)
    }

    /// Layer metadata (`upAxis`, `metersPerUnit`, `defaultPrim`, …).
    pub fn metadata(&self, key: &str) -> Option<&Value> {
        self.field("/", key)
    }

    /// Root prims in authored order.
    pub fn root_prims(&self) -> Vec<String> {
        self.children("/")
    }

    /// Child prim paths in authored order (`primChildren`).
    pub fn children(&self, prim: &str) -> Vec<String> {
        let names = self.field(prim, "primChildren").and_then(Value::as_tokens).unwrap_or_default();
        names.iter().map(|n| child_path(prim, n)).collect()
    }

    pub fn type_name(&self, prim: &str) -> Option<&str> {
        self.field(prim, "typeName").and_then(Value::as_str)
    }

    /// A property's default value, or its first time sample when it only has samples.
    pub fn attr(&self, prim: &str, name: &str) -> Option<&Value> {
        let spec = self.spec(&property_path(prim, name))?;
        match spec.field("default") {
            Some(Value::Blocked) => None,
            Some(v) => Some(v),
            None => match spec.field("timeSamples") {
                Some(Value::TimeSamples(s)) => s.first().map(|(_, v)| v).filter(|v| **v != Value::Blocked),
                _ => None,
            },
        }
    }

    /// Metadata on a property, e.g. `interpolation` on a primvar.
    pub fn attr_meta(&self, prim: &str, name: &str, key: &str) -> Option<&Value> {
        self.field(&property_path(prim, name), key)
    }

    /// Where an attribute is connected to (`inputs:x.connect = </Shader.outputs:rgb>`).
    pub fn connections(&self, prim: &str, name: &str) -> &[String] {
        self.field(&property_path(prim, name), "connectionPaths").and_then(Value::as_paths).unwrap_or_default()
    }

    /// A relationship's targets (`rel material:binding = </Mat>`).
    pub fn targets(&self, prim: &str, name: &str) -> &[String] {
        self.field(&property_path(prim, name), "targetPaths").and_then(Value::as_paths).unwrap_or_default()
    }

    pub fn has_property(&self, prim: &str, name: &str) -> bool {
        self.specs.contains_key(&property_path(prim, name))
    }
}

pub fn child_path(parent: &str, name: &str) -> String {
    if parent == "/" { format!("/{name}") } else { format!("{parent}/{name}") }
}

pub fn property_path(prim: &str, name: &str) -> String {
    format!("{prim}.{name}")
}

/// Splits `/A/B.outputs:rgb` into (`/A/B`, `outputs:rgb`).
pub fn split_property(path: &str) -> Option<(&str, &str)> {
    let slash = path.rfind('/').unwrap_or(0);
    let dot = path[slash..].find('.')? + slash;
    Some((&path[..dot], &path[dot + 1..]))
}
