use crate::error::check;
use crate::{Error, SceneLimits, Stream};

/// Colour textures are sRGB; data textures (normal, metallic/roughness, occlusion) are
/// linear. Getting this wrong is the usual cause of washed-out models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorSpace {
    Srgb,
    Linear,
}

/// Pixel layout of the texture's levels. Compressed formats stay compressed on the GPU
/// where the adapter supports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Rgba8,
    Rgba16F,
    Bc1,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
    Etc2Rgba8,
    Astc4x4,
}

impl TextureFormat {
    /// (block side in pixels, bytes per block). Uncompressed formats have 1×1 blocks.
    pub fn block(self) -> (u32, u64) {
        match self {
            TextureFormat::Rgba8 => (1, 4),
            TextureFormat::Rgba16F => (1, 8),
            TextureFormat::Bc1 | TextureFormat::Bc4 => (4, 8),
            TextureFormat::Bc3 | TextureFormat::Bc5 | TextureFormat::Bc7 | TextureFormat::Etc2Rgba8 | TextureFormat::Astc4x4 => {
                (4, 16)
            }
        }
    }

    pub fn is_compressed(self) -> bool {
        self.block().0 > 1
    }

    /// Bytes of one level of `width`×`height` pixels.
    pub fn level_bytes(self, width: u32, height: u32) -> u64 {
        let (side, bytes) = self.block();
        width.div_ceil(side) as u64 * height.div_ceil(side) as u64 * bytes
    }
}

/// A decoded texture: level 0 first, then any mips the file carried. The renderer
/// generates missing mips on the GPU.
#[derive(Debug, Clone)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub color_space: ColorSpace,
    pub levels: Vec<Stream<u8>>,
}

impl TextureData {
    pub fn rgba8(width: u32, height: u32, color_space: ColorSpace, pixels: impl Into<Stream<u8>>) -> Self {
        Self { width, height, format: TextureFormat::Rgba8, color_space, levels: vec![pixels.into()] }
    }

    pub fn level_size(&self, level: u32) -> (u32, u32) {
        ((self.width >> level).max(1), (self.height >> level).max(1))
    }

    pub fn byte_len(&self) -> u64 {
        self.levels.iter().map(Stream::byte_len).sum()
    }

    pub fn validate(&self, limits: &SceneLimits) -> crate::Result<TextureInfo> {
        if self.width == 0 || self.height == 0 {
            return Err(Error::BadTexture("zero size"));
        }
        check("texture side", self.width.max(self.height) as u64, limits.max_texture_side as u64)?;
        // GPUs (and wgpu's validation) require block-compressed level 0 to be whole blocks.
        let side = self.format.block().0;
        if !self.width.is_multiple_of(side) || !self.height.is_multiple_of(side) {
            return Err(Error::BadTexture("block-compressed size isn't a multiple of the block size"));
        }
        let full_chain = 32 - self.width.max(self.height).leading_zeros();
        if self.levels.is_empty() || self.levels.len() > full_chain as usize {
            return Err(Error::BadTexture("level count"));
        }
        for (i, level) in self.levels.iter().enumerate() {
            let (w, h) = self.level_size(i as u32);
            let expected = self.format.level_bytes(w, h);
            if level.byte_len() != expected {
                return Err(Error::StreamLength { stream: "texture level", len: level.len(), expected: expected as usize });
            }
        }
        Ok(TextureInfo {
            width: self.width,
            height: self.height,
            format: self.format,
            color_space: self.color_space,
            levels: self.levels.len() as u32,
            bytes: self.byte_len(),
        })
    }
}

/// What the scene remembers about a texture after its data has been uploaded and freed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureInfo {
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub color_space: ColorSpace,
    pub levels: u32,
    pub bytes: u64,
}
