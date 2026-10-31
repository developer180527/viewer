//! Textures on the GPU, and the per-material bind groups that point at them.
//!
//! Milestone 1 binds one texture per material, the base colour, which clay multiplies
//! into the material colour. The other slots arrive with the authored shading models.

use rendererx_scene::{ColorSpace, Material, Slot, TextureData, TextureFormat, TextureId};

/// What every material bind group is built from; one per renderer.
pub(crate) struct MaterialBinding {
    pub layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// For materials without a base colour texture (or whose texture isn't here yet).
    white: wgpu::TextureView,
    pub white_group: wgpu::BindGroup,
}

impl MaterialBinding {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rendererx material"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
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
        let white = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rendererx white"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            white.as_image_copy(),
            &[255; 4],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let white = white.create_view(&Default::default());
        let white_group = group(device, &layout, &sampler, &white);
        Self { layout, sampler, white, white_group }
    }

    /// A bind group sampling `base` (white when `None`).
    pub fn group(&self, device: &wgpu::Device, base: Option<&wgpu::TextureView>) -> wgpu::BindGroup {
        group(device, &self.layout, &self.sampler, base.unwrap_or(&self.white))
    }
}

fn group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, view: &wgpu::TextureView) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rendererx material"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

/// The texture clay samples for a material: its base colour map.
pub(crate) fn base_texture(material: &Material) -> Option<TextureId> {
    material.textures.iter().find(|(slot, _)| *slot == Slot::BASE_COLOR).map(|(_, t)| *t)
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
