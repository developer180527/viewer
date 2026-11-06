use std::borrow::Cow;

use glam::{Vec2, Vec3, Vec4};

use crate::TextureId;

/// Which shading model draws a material (docs/design/3d-renderer.md §8). An open set: the
/// built-ins have constants, external models register their own names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShadingId(Cow<'static, str>);

impl ShadingId {
    pub const CLAY: Self = Self::builtin("clay");
    pub const UNLIT: Self = Self::builtin("unlit");
    pub const LAMBERT: Self = Self::builtin("lambert");
    pub const BLINN_PHONG: Self = Self::builtin("blinn_phong");
    pub const PBR: Self = Self::builtin("pbr");
    pub const MTOON: Self = Self::builtin("mtoon");
    pub const MATCAP: Self = Self::builtin("matcap");
    pub const WIREFRAME: Self = Self::builtin("wireframe");

    const fn builtin(name: &'static str) -> Self {
        Self(Cow::Borrowed(name))
    }

    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a material's shading model came from, so the host can say so in its UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShadingSource {
    /// The file names it (glTF unlit, VRM MToon, FBX Phong…).
    FromFile,
    /// Guessed from the parameters present (e.g. MTL with `Pr`/`Pm` → PBR).
    Inferred,
    /// The file has no materials.
    Default,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamValue {
    Float(f32),
    Vec2(Vec2),
    Vec3(Vec3),
    Vec4(Vec4),
    Int(i32),
}

impl ParamValue {
    fn is_finite(&self) -> bool {
        match self {
            ParamValue::Float(v) => v.is_finite(),
            ParamValue::Vec2(v) => v.is_finite(),
            ParamValue::Vec3(v) => v.is_finite(),
            ParamValue::Vec4(v) => v.is_finite(),
            ParamValue::Int(_) => true,
        }
    }
}

/// Values for the parameters a shading model declares, by name. Unknown names are
/// ignored by the renderer; missing ones take the model's defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParamBlock(Vec<(Cow<'static, str>, ParamValue)>);

impl ParamBlock {
    pub fn set(&mut self, name: impl Into<Cow<'static, str>>, value: ParamValue) {
        let name = name.into();
        match self.0.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = value,
            None => self.0.push((name, value)),
        }
    }

    pub fn get(&self, name: &str) -> Option<ParamValue> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, ParamValue)> {
        self.0.iter().map(|(n, v)| (&**n, *v))
    }
}

/// A texture binding point of a shading model.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Slot(Cow<'static, str>);

impl Slot {
    pub const BASE_COLOR: Self = Self(Cow::Borrowed("base_color"));
    pub const NORMAL: Self = Self(Cow::Borrowed("normal"));
    pub const METALLIC_ROUGHNESS: Self = Self(Cow::Borrowed("metallic_roughness"));
    pub const OCCLUSION: Self = Self(Cow::Borrowed("occlusion"));
    pub const EMISSIVE: Self = Self(Cow::Borrowed("emissive"));

    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlphaMode {
    Opaque,
    /// Discard below the cutoff.
    Mask(f32),
    Blend,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Material {
    pub name: String,
    pub shading: ShadingId,
    pub source: ShadingSource,
    pub params: ParamBlock,
    pub textures: Vec<(Slot, TextureId)>,
    pub alpha: AlphaMode,
    pub double_sided: bool,
}

impl Material {
    pub fn new(shading: ShadingId, source: ShadingSource) -> Self {
        Self {
            name: String::new(),
            shading,
            source,
            params: ParamBlock::default(),
            textures: Vec::new(),
            alpha: AlphaMode::Opaque,
            double_sided: false,
        }
    }

    /// What files without materials get (STL, PLY…), and what any material is drawn
    /// with until its own pipeline is ready.
    pub fn clay() -> Self {
        Self::new(ShadingId::CLAY, ShadingSource::Default)
    }

    pub fn with_param(mut self, name: impl Into<Cow<'static, str>>, value: ParamValue) -> Self {
        self.params.set(name, value);
        self
    }

    pub fn with_texture(mut self, slot: Slot, texture: TextureId) -> Self {
        self.textures.push((slot, texture));
        self
    }

    pub(crate) fn validate(&self) -> crate::Result<()> {
        if let AlphaMode::Mask(cutoff) = self.alpha
            && !cutoff.is_finite()
        {
            return Err(crate::Error::BadMaterial("alpha cutoff is not finite"));
        }
        if self.params.0.iter().any(|(_, v)| !v.is_finite()) {
            return Err(crate::Error::BadMaterial("a parameter is not finite"));
        }
        Ok(())
    }
}
