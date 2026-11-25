//! libviewer's views on a GPU surface, for every shell: the same fonts, the same GPU
//! host (textures, RendererX for 3D and its shader-compile thread) and the same page
//! views, so a PDF page or a model is drawn by the same code in the libgui app and
//! in the SwiftUI shell.
//!
//! - [`fonts`]: Inter, Hack, their bold and italic faces, and the system's fonts behind
//!   them; [`fonts::new_ui`] makes a libgui `Ui` with all of it.
//! - [`Gpu`] and [`Rx`]: libviewer's [`libviewer::Host`] on a wgpu device.
//! - [`views`]: a page's view state and how it is shown.
//! - [`Canvas`]: one page drawn into a surface another UI owns (a `CAMetalLayer` in the
//!   SwiftUI shell), with its input pushed in by that UI.

pub mod chrome;
pub mod fonts;
mod gpu;
pub mod views;
mod canvas;

pub use canvas::{Canvas, Drawn};
pub use gpu::{Gpu, GpuContext, GpuUploader, Rx, TexturePool, Wake, open_device, probe_texture_compression};
