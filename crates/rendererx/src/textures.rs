//! Textures on the GPU, and the per-material bind groups that point at them.
//!
//! Every material gets one bind group: a small uniform block (the factors) and its
//! five glTF maps, with 1×1 defaults for missing ones. All shading models share the
//! layout and use what they need (clay only the base colour).

use scene_data::{AlphaMode, ColorSpace, Material, ParamValue, ShadingId, Slot, TextureData, TextureFormat, TextureId};
use wgpu::util::DeviceExt;

/// The fragment stage a material is drawn with (docs/design/3d-renderer.md §8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Model {
    Clay,
    Pbr,
    Unlit,
}

impl Model {
    /// Built-in models not written yet (mtoon, matcap, wireframe) draw as clay. FBX
    /// Lambert and Phong draw as PBR from the factors ufbx translates them to.
    pub fn of(material: Option<&Material>) -> Self {
        let Some(m) = material else { return Model::Clay };
        if m.shading == ShadingId::PBR || m.shading == ShadingId::LAMBERT || m.shading == ShadingId::BLINN_PHONG {
            Model::Pbr
        } else if m.shading == ShadingId::UNLIT {
            Model::Unlit
        } else {
            Model::Clay
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    /// Fragment entry points: (writes sRGB bytes, writes linear for the MSAA path).
    pub fn entry_points(self) -> (&'static str, &'static str) {
        match self {
            Model::Clay => ("fs_clay", "fs_clay_linear"),
            Model::Pbr => ("fs_pbr", "fs_pbr_linear"),
            Model::Unlit => ("fs_unlit", "fs_unlit_linear"),
        }
    }
}

/// The map slots in binding order (bindings 1–5).
pub(crate) const SLOTS: [Slot; 5] = [Slot::BASE_COLOR, Slot::METALLIC_ROUGHNESS, Slot::NORMAL, Slot::OCCLUSION, Slot::EMISSIVE];

/// What every material bind group is built from; one per renderer.
pub(crate) struct MaterialBinding {
    pub layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    white: wgpu::TextureView,
    /// (128, 128, 255): a normal map that changes nothing.
    flat_normal: wgpu::TextureView,
    /// For nodes without a material.
    pub default_group: wgpu::BindGroup,
}

impl MaterialBinding {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rendererx material"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(UNIFORM_BYTES),
                    },
                    count: None,
                },
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
                texture_entry(5),
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rendererx material"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let texel = |label, rgba: [u8; 4]| {
            let size = wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 };
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let layout = wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) };
            queue.write_texture(t.as_image_copy(), &rgba, layout, size);
            t.create_view(&Default::default())
        };
        let white = texel("rendererx white", [255; 4]);
        let flat_normal = texel("rendererx flat normal", [128, 128, 255, 255]);
        let defaults = Defaults { white: &white, flat_normal: &flat_normal };
        let default_uniforms = uniform_buffer(device, None);
        let default_group = make_group(device, &layout, &sampler, defaults, &default_uniforms, [None; 5]);
        Self { layout, sampler, white, flat_normal, default_group }
    }

    /// The factors block for `material` (defaults when `None`), shared by its groups.
    pub fn uniforms(&self, device: &wgpu::Device, material: Option<&Material>) -> wgpu::Buffer {
        uniform_buffer(device, material)
    }

    /// A bind group over `uniforms` with the given maps, in [`SLOTS`] order; missing
    /// maps use the 1×1 defaults (all of them: the group for meshes without uvs, which
    /// would otherwise sample one corner texel of every map).
    pub fn group(&self, device: &wgpu::Device, uniforms: &wgpu::Buffer, maps: [Option<&wgpu::TextureView>; 5]) -> wgpu::BindGroup {
        let defaults = Defaults { white: &self.white, flat_normal: &self.flat_normal };
        make_group(device, &self.layout, &self.sampler, defaults, uniforms, maps)
    }
}

fn uniform_buffer(device: &wgpu::Device, material: Option<&Material>) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("rendererx material"),
        contents: bytemuck::cast_slice(&uniforms(material)),
        usage: wgpu::BufferUsages::UNIFORM,
    })
}

#[derive(Clone, Copy)]
struct Defaults<'a> {
    white: &'a wgpu::TextureView,
    flat_normal: &'a wgpu::TextureView,
}

fn make_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    defaults: Defaults<'_>,
    uniforms: &wgpu::Buffer,
    maps: [Option<&wgpu::TextureView>; 5],
) -> wgpu::BindGroup {
    let views: [&wgpu::TextureView; 5] =
        std::array::from_fn(|i| maps[i].unwrap_or(if SLOTS[i] == Slot::NORMAL { defaults.flat_normal } else { defaults.white }));
    let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }];
    entries.extend(views.iter().enumerate().map(|(i, v)| wgpu::BindGroupEntry { binding: i as u32 + 1, resource: wgpu::BindingResource::TextureView(v) }));
    entries.push(wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(sampler) });
    // The bind group keeps the uniform buffer alive.
    device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("rendererx material"), layout, entries: &entries })
}

const UNIFORM_BYTES: u64 = 32;

/// `MaterialUniforms` in common.wgsl: emissive (rgb) + normal scale, then metallic,
/// roughness, occlusion strength and alpha cutoff (negative: opaque).
fn uniforms(material: Option<&Material>) -> [f32; 8] {
    let float = |name: &str, default: f32| match material.and_then(|m| m.params.get(name)) {
        Some(ParamValue::Float(v)) if v.is_finite() => v,
        _ => default,
    };
    let emissive = match material.and_then(|m| m.params.get("emissive")) {
        Some(ParamValue::Vec3(v)) => v.to_array(),
        Some(ParamValue::Vec4(v)) => v.truncate().to_array(),
        _ => [0.0; 3],
    };
    let cutoff = match material.map(|m| m.alpha) {
        Some(AlphaMode::Mask(c)) => c,
        // No blending yet: drawn as a cutout, so fully clear parts still vanish.
        Some(AlphaMode::Blend) => 0.05,
        _ => -1.0,
    };
    [
        emissive[0],
        emissive[1],
        emissive[2],
        float("normal_scale", 1.0),
        float("metallic", 0.0),
        float("roughness", 0.5),
        float("occlusion_strength", 1.0),
        cutoff,
    ]
}

/// The texture a material uses for `slot`.
pub(crate) fn texture_for(material: &Material, slot: &Slot) -> Option<TextureId> {
    material.textures.iter().find(|(s, _)| s == slot).map(|(_, t)| *t)
}

pub(crate) struct GpuTexture {
    pub view: wgpu::TextureView,
    pub bytes: u64,
    /// Scene revision of the data that was uploaded.
    pub revision: u64,
}

/// The wgpu format for a texture, or why this device can't hold it.
fn format(data: &TextureData, features: wgpu::Features) -> Result<wgpu::TextureFormat, String> {
    use wgpu::TextureFormat as W;
    let srgb = data.color_space == ColorSpace::Srgb;
    let pick = |s: W, l: W| if srgb { s } else { l };
    let need = |feature: wgpu::Features, f: W| {
        if features.contains(feature) { Ok(f) } else { Err(format!("{:?} textures aren't supported by this GPU", data.format)) }
    };
    match data.format {
        TextureFormat::Rgba8 => Ok(pick(W::Rgba8UnormSrgb, W::Rgba8Unorm)),
        TextureFormat::Rgba16F => Ok(W::Rgba16Float),
        TextureFormat::Bc1 => need(wgpu::Features::TEXTURE_COMPRESSION_BC, pick(W::Bc1RgbaUnormSrgb, W::Bc1RgbaUnorm)),
        TextureFormat::Bc3 => need(wgpu::Features::TEXTURE_COMPRESSION_BC, pick(W::Bc3RgbaUnormSrgb, W::Bc3RgbaUnorm)),
        TextureFormat::Bc4 => need(wgpu::Features::TEXTURE_COMPRESSION_BC, W::Bc4RUnorm),
        TextureFormat::Bc5 => need(wgpu::Features::TEXTURE_COMPRESSION_BC, W::Bc5RgUnorm),
        TextureFormat::Bc7 => need(wgpu::Features::TEXTURE_COMPRESSION_BC, pick(W::Bc7RgbaUnormSrgb, W::Bc7RgbaUnorm)),
        TextureFormat::Etc2Rgba8 => need(wgpu::Features::TEXTURE_COMPRESSION_ETC2, pick(W::Etc2Rgba8UnormSrgb, W::Etc2Rgba8Unorm)),
        TextureFormat::Astc4x4 => need(
            wgpu::Features::TEXTURE_COMPRESSION_ASTC,
            W::Astc { block: wgpu::AstcBlock::B4x4, channel: if srgb { wgpu::AstcChannel::UnormSrgb } else { wgpu::AstcChannel::Unorm } },
        ),
    }
}

/// Uploads every level the data carries (decoders generate mips on their own threads).
pub(crate) fn upload_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    limits: &wgpu::Limits,
    data: &TextureData,
    revision: u64,
) -> Result<GpuTexture, String> {
    let format = format(data, device.features())?;
    let max = limits.max_texture_dimension_2d;
    if data.width > max || data.height > max {
        return Err(format!("{}×{} is over this GPU's {max}-pixel limit", data.width, data.height));
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rendererx texture"),
        size: wgpu::Extent3d { width: data.width, height: data.height, depth_or_array_layers: 1 },
        mip_level_count: data.levels.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let (side, block_bytes) = data.format.block();
    for (level, bytes) in data.levels.iter().enumerate() {
        let (w, h) = data.level_size(level as u32);
        let (blocks_x, blocks_y) = (w.div_ceil(side), h.div_ceil(side));
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: level as u32, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytes.as_slice(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(blocks_x * block_bytes as u32), rows_per_image: Some(blocks_y) },
            // Compressed levels are copied in whole blocks (a 2×2 level is one 4×4 block).
            wgpu::Extent3d { width: blocks_x * side, height: blocks_y * side, depth_or_array_layers: 1 },
        );
    }
    Ok(GpuTexture { view: texture.create_view(&Default::default()), bytes: data.byte_len(), revision })
}
