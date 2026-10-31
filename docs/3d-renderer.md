# RendererX — a reusable 3D renderer on wgpu

Status: `rendererx-scene` implemented (milestone 1, step 1); `rendererx` not started. Updated 2026-10-31 for the decided architecture (PLAN.md §12): scenes are built without the GPU, one shared device, many windows, and edits as operations.

The product name is **RendererX**; the Cargo package and Rust crate are `rendererx`
(Rust warns on non-snake-case crate names, and lowercase names avoid case clashes on crates.io and in paths).

A small, fast 3D renderer for **showing** models (viewers, previews, thumbnails, basic editors).
The Viewer app is its first user. It is meant to be used by other apps too, so it knows
nothing about Viewer, libgui or any file format.

---

## 1. Goals and non-goals

**Goals**

1. **First frame fast.** Geometry on screen before textures and the authored shaders are ready; everything else streams in. Budget: §11.
2. **Show it as the artist made it.** Shading is per material and comes from the file (unlit, Lambert, Blinn-Phong, PBR, toon/MToon), never "PBR for everything".
3. **Swappable shaders.** Built-in and external shading models go through one registry and one interface.
4. **Runs wherever wgpu runs:** Vulkan, Metal, D3D12, GL / GLES 3 (and WebGPU later). The built-in shaders stay within the GLES 3 / downlevel limits.
5. **Embeddable.** Uses the host's `wgpu::Device` and `Queue`, draws into a texture the host gives it, and owns no window, event loop or thread pool.
6. **Safe on hostile input.** Hard limits on triangles, textures, memory and node depth.

**Non-goals**

- A game engine: no physics, scripting, audio, ECS or animation state machines.
- Offline / path-traced rendering.
- File parsing. Loaders live outside (§3); the renderer takes an already-built scene.
- Reproducing engine shader graphs (Unity, Unreal, Blender nodes). Files don't contain them.

---

## 2. Crate layout

```
crates/
  rendererx-scene/        the CPU scene (§5): plain data, no wgpu. Loaders and viewer-core depend on this only
  rendererx/              the renderer. Depends on wgpu, glam, bytemuck, and nothing else heavy
    src/
      lib.rs             public API (§4)
      scene.rs           GPU-side scene: what was uploaded (§5)
      upload.rs          streaming GPU upload (§7)
      shading/           shader registry, built-in WGSL, material interface (§8)
      passes/            the passes of a frame (§9)
      lighting.rs        lights, SH ambient, environment map (§10)
      camera.rs          camera + optional orbit controller (feature `controls`)
      pick.rs            ray picking (feature `pick`, for editors)
      soft/              CPU fallback rasteriser (feature `soft`, §13)
  rendererx-gltf/         optional: glTF/GLB → rendererx scene (zero-copy, §6)
  viewer-fmt-3d/         Viewer's loaders (glTF, OBJ, STL, PLY, FBX via ufbx) → rendererx scene
  app/                   Viewer: shows the target texture as a libgui image
```

`rendererx` must **not** depend on `viewer-core`, libgui, `image` or any format crate. The adapters (`rendererx-gltf`, `viewer-fmt-3d`) depend on **`rendererx-scene` only**, never on `rendererx` or wgpu: a loader must be able to run before any GPU exists, in a worker process, or in a WASM sandbox (PLAN.md §12.4).

**Cargo features** (all off by default except `wgsl-builtin`):

| Feature | Adds |
|---|---|
| `wgsl-builtin` | the built-in shading models (§8.3) |
| `controls` | orbit / pan / zoom camera controller, mouse and touch |
| `pick` | ray picking against meshes, for selection in editors |
| `hot-reload` | rebuild pipelines when WGSL files change (dev builds) |
| `simplify` | background LOD generation through `meshopt` |
| `soft` | CPU fallback rasteriser |

---

## 3. Responsibilities

| Who | Does |
|---|---|
| Host app | Owns window, device, queue, event loop, threads. Calls `render()` when needed, shows the target. |
| Loader (adapter) | Parses a file into a CPU `SceneBuilder` (`rendererx-scene`), geometry first, then textures as they decode. Needs no GPU. |
| `rendererx` | Uploads CPU scenes, GPU resources, shader registry, pipelines, the frame, camera, picking. |

---

## 4. Public API (sketch)

```rust
// Loader thread, at launch, before any window or GPU exists (rendererx-scene: no wgpu).
let (scene, mut builder) = CpuScene::new();       // `scene` is a cheap handle (Arc) the host keeps
builder.set_bounds(aabb);                         // optional, frames the camera before any mesh exists
let mesh = builder.add_mesh(MeshData { .. })?;    // may arrive in chunks
let mat  = builder.add_material(Material::toon(MToonParams { .. }));
builder.add_node(Node { mesh, material: mat, transform, parent: None });
builder.set_texture(tex_id, TextureData::Rgba8 { .. }); // any time later, any thread
builder.finish();                                 // "no more geometry"; textures may still come

// Once per process, on the shared device (PLAN.md §12.2). Cheap: compiles `clay` only.
let mut r = Renderer::new(&device, &queue, RendererDesc {
    target_format: wgpu::TextureFormat::Rgba8Unorm,
    msaa: Msaa::Auto,                 // 4x on desktop, 1–2x on weak mobile GPUs
    limits: SceneLimits::default(),   // §12
    shaders: ShaderRegistry::builtin(),
})?;

// Once per scene, whenever the GPU is ready. Streams what the builder has so far and
// keeps following it, so it doesn't matter whether loading or the GPU finished first.
let gpu_scene = r.attach(&scene);

// Per view (one per window showing the model). Owns its camera, target and options.
let mut view = r.create_view(ViewDesc { size, .. });

// Per frame, only when `r.needs_redraw(&view)`.
r.render(&gpu_scene, &mut view, &camera, RenderOptions { shading: ShadingOverride::AsAuthored, .. });
let tex = view.target();              // host shows it (libgui: a registered texture)
Rules:
- **All `&self` work that can be threaded is `Send + Sync`.** Pipeline compilation and uploads may run on host-provided threads (`r.compile_pending(&pool)`).
- **No hidden threads, no globals.** Background work is returned as jobs the host runs (`r.take_jobs()`), so the host keeps control of its thread pool.
- **One `Renderer` per device, many views.** Every window shares the renderer, its pipeline cache and its uploaded scenes. A model open in two windows is uploaded once; each window has its own `View` (camera, render target, MSAA buffers, options).
- **Output contract (for libgui):** the view target is RGBA8, **sRGB-encoded, straight alpha**. The background is opaque by default; with a transparent background (thumbnails), alpha is straight and colour is not premultiplied.
- **`render()` never blocks** on a texture or pipeline that isn't ready; it draws with what exists (§7.3).
- Errors are values (`thiserror`), never panics, including for bad shaders and limit violations.

---

## 5. Scene model

Kept small and close to glTF, because glTF already maps the formats we care about. These types live in **`rendererx-scene`** and are plain data: `Send`, no GPU handles, and serialisable, so a scene can be built in a parser worker process and handed over in shared memory (PLAN.md §12.4).

```rust
pub struct MeshData {
    pub positions: Stream<Vec3>,           // owned, or a view into shared bytes (mmap / shared memory)
    pub normals:   Option<Stream<Vec3>>,   // generated (flat or smooth) when missing
    pub uvs:       Option<Stream<Vec2>>,
    pub tangents:  Option<Stream<Vec4>>,   // generated (MikkTSpace) only if a normal map needs them
    pub colors:    Option<Stream<Vec4>>,
    pub indices:   Indices,                // None (non-indexed) | U16 | U32
    pub topology:  Topology,               // Triangles | Lines | Points
    pub bounds:    Option<Aabb>,
}

pub struct Material {
    pub name: String,
    pub shading: ShadingId,        // which shading model (§8); from the file
    pub source: ShadingSource,     // FromFile | Inferred | Default, shown in the host's UI
    pub params: ParamBlock,        // values for that model's declared parameters
    pub textures: Vec<(Slot, TextureId)>,
    pub alpha: AlphaMode,          // Opaque | Mask(cutoff) | Blend
    pub double_sided: bool,
}

// A group node has no mesh; `material: None` draws with clay. Parents must exist first (no cycles).
pub struct Node { pub mesh: Option<MeshId>, pub material: Option<MaterialId>, pub transform: Mat4, pub parent: Option<NodeId> }
```

- `Stream<T>` replaces a borrowed `Cow<'a, [T]>`: a scene outlives the loader call, so zero-copy input holds the bytes alive (`Arc<Mmap>`, a shared-memory region) instead of borrowing them. Misaligned bytes are copied once.
- Readers follow the scene by revision: every entry records the revision that last changed it, and the renderer uploads only entries newer than what it has seen. `set_on_change` lets the host wake its event loop.
- Texture ids are reserved before decoding (`reserve_texture`), so materials can reference them at once; decoder threads set them later through a `TextureSink`.

- Colour textures are tagged sRGB, data textures (normal, metallic/roughness, occlusion) are linear. Getting this wrong is the main cause of washed-out models, so the type carries it: `TextureData { color_space: ColorSpace, .. }`.
- **Instancing is automatic:** nodes that share mesh and material are batched.
- Skinning and morph targets are **out of v1**, but `MeshData` has room for joint and weight streams later.

---

## 6. Loaders and zero-copy input

The renderer doesn't parse files, but its input types are shaped so loaders can be fast:

- `Cow` streams let a GLB loader **borrow vertex data straight from the mmap** when it runs in-process. The upload copies it once, into the GPU staging buffer. A loader in a worker process writes streams into a shared-memory buffer instead, which the app maps without copying.
- Streams that need conversion (FBX via `ufbx`, OBJ, STL) are converted on the loader's threads and handed over as owned data.
- Loaders should call `set_bounds` early (glTF accessors carry min/max), so the camera frames the model before the meshes arrive.
- Texture decoding happens in the loader (it owns the image decoders). The renderer accepts RGBA8, RGBA16F and **GPU-compressed blocks** (BC1–7, ETC2, ASTC) when the device supports them.

Shading as authored, per format (the adapters fill `Material::shading`):

| Source | Shading model |
|---|---|
| glTF core | `pbr` |
| glTF `KHR_materials_unlit` | `unlit` |
| glTF `KHR_materials_{clearcoat,sheen,transmission,emissive_strength,specular,ior}` | `pbr` with those layers |
| VRM `VRMC_materials_mtoon` | `mtoon` |
| FBX Lambert / Phong | `lambert` / `blinn_phong` |
| FBX Standard Surface / Stingray PBS / 3ds Max Physical | `pbr` |
| OBJ/MTL `illum` + `Kd/Ks/Ns`; `Pr/Pm` present | `blinn_phong`; `pbr` |
| STL, PLY, no materials | `clay` (vertex colours if present) |

---

## 7. Streaming and the first frame

### 7.1 Order of work

```
host: device ready ──► Renderer::new ──► compile `clay` pipeline (the only one on the critical path)
loader thread:  parse geometry ─► add_mesh (chunks) ─► finish
                                   │
renderer:        upload chunks ────┴─► FIRST FRAME (clay / vertex colours, SH ambient)
background jobs: decode textures (loader) ─► upload smallest mip first ─► swap in
                 compile authored pipelines (pbr, mtoon, …) ─► swap in per material
                 environment map prefilter ─► swap SH ambient for full IBL
```

### 7.2 Upload

- One ring **staging buffer** (e.g. 32 MB) reused across frames; big meshes are uploaded in chunks with a per-frame byte budget, so a 500 MB model never stalls a frame.
- Vertex streams go into a few large buffers (sub-allocated), not one buffer per mesh. That means fewer bind changes and fewer allocations.
- Mipmaps are generated on the GPU (a render pass per level, which works on GLES 3) for textures that arrive without them.

### 7.3 What `render()` does with missing pieces

| Missing | Drawn as |
|---|---|
| Mesh chunk not uploaded yet | skipped this frame |
| Texture not ready | material's constant colour / factor |
| Authored pipeline still compiling | `clay` pipeline with the material's base colour |
| Environment map | SH ambient (9 coefficients, computed instantly) |

Each swap-in sets `needs_redraw`, so the host draws one new frame and then goes idle again.

---

## 8. Shading system (swappable shaders)

### 8.1 The contract every shading model follows

| Binding | Contents | Owner |
|---|---|---|
| vertex inputs | position, normal, uv, tangent, colour (missing streams read as defaults) | mesh |
| `@group(0)` | camera, lights, SH ambient, environment map, time | renderer, per frame |
| `@group(1)` | the material's parameter block + its textures | material |
| `@group(2)` | per-draw: model matrix, node id (for picking) | renderer |

Shared WGSL helpers (colour-space conversion, BRDF terms, tonemapping, SH evaluation) are joined in with a small `#import` step (`naga_oil` or our own; WGSL has no `#include`).

### 8.2 A shading model is a package

```rust
pub struct ShadingModel {
    pub id: &'static str,                  // "pbr", "mtoon", …
    pub wgsl: Cow<'static, str>,
    pub params: &'static [ParamDecl],      // name, type, default: drives ParamBlock layout and editor UI
    pub slots: &'static [TextureSlot],     // base_color, normal, shade, matcap, …
    pub passes: &'static [PassDecl],       // e.g. mtoon: [Main, Outline]
    pub lighting: LightingStyle,           // Pbr (IBL + tonemap) | Artistic (no tonemap, key light)
}
```

- The registry is a **static table** (built-ins are `include_str!`); nothing is scanned at startup.
- Hosts can register more (`registry.add(model)`), including WGSL loaded at runtime. naga validates it before use; a model that breaks the contract or the device limits is rejected with a readable error.
- Runtime-loaded shaders can still be slow (validation can't catch a huge loop). Hosts should only load trusted ones until they have a sandbox.

### 8.3 Built-in models

| id | Look | Notes |
|---|---|---|
| `clay` | neutral matte | the only pipeline compiled at startup; also "no materials" |
| `unlit` | exact authored colour | no lighting, no tonemapping |
| `lambert` | diffuse only | FBX / MTL classic |
| `blinn_phong` | diffuse + specular | FBX Phong, MTL `Ks/Ns` |
| `pbr` | metallic/roughness | glTF core + KHR extensions |
| `mtoon` | toon ramp, shade colour, rim, matcap, outline | outline = second pass, inflated back faces |
| `matcap` | sculpt-style | inspection mode |
| `wireframe` | edges | barycentric in-shader on GLES; line mode where supported |

### 8.4 Override

`RenderOptions::shading` is `AsAuthored` (default) or `Force(ShadingId)`, which lets the host's "Shading" menu switch models without changing the scene. `Material::source` tells the UI where each choice came from ("MToon (from file)").

### 8.5 Pipeline cache

Key: `(shading id, pass, vertex layout, alpha mode, double-sided, msaa, target format)`.
- Pipelines are compiled on job threads and swapped in when ready (§7.3).
- A `wgpu::PipelineCache` is used where the backend supports it (Vulkan); the host decides where its blob is stored.
- With `hot-reload`, changed WGSL invalidates only the entries using that model.

---

## 9. A frame

Passes, each skipped when it has nothing to do:

1. **Cull**: per-node AABB vs frustum, on the CPU (scenes here are small enough).
2. **Opaque + mask**: sorted by pipeline → material → mesh to minimise state changes; instanced.
3. **Outline**: models with an `Outline` pass (mtoon).
4. **Blend**: sorted back-to-front per node.
5. **Overlays**: wireframe, bounds, selection highlight, grid (optional).
6. **Resolve + output**: MSAA resolve, then tonemap. Tonemapping applies to `Pbr`-style materials only; `Artistic` ones (unlit, toon) keep their exact colours.

Rendering is **on demand**: `needs_redraw()` is true only after camera input, a finished upload or compile, or an option change. An idle viewer costs zero GPU time.

---

## 10. Lighting and colour

- Linear-space lighting; sRGB decode on colour textures and sRGB encode on output.
- Default rig: one key directional light + ambient. PBR adds image-based lighting from a small built-in environment (prefiltered on first use, in the background); until then SH ambient.
- `LightingStyle::Artistic` models ignore the environment and tonemapping, so stylised art shows exactly as painted.
- Tonemapper for PBR: Khronos PBR Neutral (keeps base colours recognisable, unlike ACES).
- Lights from the file (glTF `KHR_lights_punctual`, FBX lights) are used when present, capped at a small number; v1 has **no shadows** (a later feature flag).

---

## 11. Performance budgets

Warm start, mid-range laptop, model ≤ 1M triangles / 50 MB:

| Milestone | Budget |
|---|---|
| `Renderer::new` + `clay` pipeline | ≤ 15 ms (overlaps device creation) |
| geometry parsed (loader) | ≤ 60 ms |
| first frame with geometry | ≤ 150 ms from launch |
| textures complete | ≤ 500 ms |
| frame time while orbiting | ≤ 4 ms GPU, ≤ 1 ms CPU |
| idle | 0 frames |

Heavy models: above `SceneLimits::interactive_triangles`, the `simplify` feature builds a decimated LOD in the background, which is used while the camera moves; the full mesh is drawn when it stops.

The host's tracing hooks (`r.set_trace(fn)`) report: `pipeline clay ready`, `first mesh uploaded`, `first frame`, `textures complete`, `authored pipelines ready`. Viewer prints these under `--trace-startup` and benchmarks them in CI.

### 11.1 Memory

Memory is budgeted like time. Rules from the first line of code:
- **One copy per file.** The GPU scene for a file is shared by every window showing it; nothing is duplicated per view.
- **Free the CPU copy after upload.** Vertex, index and texture bytes are dropped once on the GPU, unless an editor holds the document open for editing (§14), which keeps the CPU side as the source of truth.
- **A view owns only what is window-sized:** depth buffer, colour target and its output texture. A hidden or minimised window frees them and recreates them when shown.
- **No fixed pools or arenas sized up front.** Memory grows with the scene and is returned when the file closes (`detach`).
- **Compressed textures** (KTX2/Basis → BC/ASTC/ETC2) stay compressed on the GPU where the adapter supports it; mip levels above what the screen can show are not uploaded.
- **Reporting:** `r.memory_report()` returns GPU bytes by kind (buffers, textures, targets) per scene and per view, built on wgpu's allocator report. Viewer checks it against budgets in CI.

| Case | Budget (on top of the empty app) |
|---|---|
| small glTF (≤ 5 MB file) | ≤ 2× file size |
| each extra window on the same file | ≤ window-sized targets only |
| file closed | back to the empty-app number |

---

## 12. Limits (hostile files)

`SceneLimits` (defaults, all configurable): max triangles (50M), max vertices (50M), max texture side (16384, capped at the device limit), max total GPU memory (e.g. 2 GB or 50% of the adapter's budget), max node depth (256), max nodes (1M).
Going over a limit returns `Error::LimitExceeded { what, limit }`; the host can still show what was accepted with a warning. Index ranges are validated against vertex counts before upload, so a bad file can never make the GPU read out of bounds.

---

## 13. Platforms and fallback

- **Backends:** everything wgpu supports. Built-ins use only downlevel-safe features (no storage buffers in vertex shaders, ≤ 16 textures per stage, no compute required) so the GL / GLES 3 backend works.
- **Mobile:** MSAA `Auto` drops to 2× or 1×; on-demand rendering saves battery; touch handled by `controls`.
- **No usable GPU:** the `soft` feature rasterises the scene on the CPU (flat or clay shading, depth buffer, multithreaded tiles), so a model always shows *something*. Same `Scene`, same API, different `render_soft()` call.

---

## 14. Editing support (for the basic editors later)

The renderer stays a viewer, but leaves the hooks editors need. Editing follows the app's one editing model (PLAN.md §12.5): **views never mutate the scene; they send operations.**
- `pick(ray) -> Option<Hit { node, triangle, position }>` (feature `pick`).
- Scene operations live in `rendererx-scene`, so they are plain data: `SetTransform { node, from, to }`, `SetMaterial { node, from, to }`, `SetParam { material, name, from, to }`, `AddNode`, `RemoveNode`. Each carries what is needed to reverse it (undo/redo).
- Applying an operation to the CPU scene marks only what changed as dirty; every attached GPU scene re-uploads just that on the next frame. Two windows showing the same model stay in sync for free.
- Selection highlight and a transform gizmo drawn in the overlay pass (per view).
- Saving is not the renderer's job: the host writes the file (e.g. Viewer exports glTF from the CPU scene).

## 15. Testing

- **Golden images:** headless rendering (wgpu on llvmpipe / WARP in CI, plus the `soft` path) of each built-in shading model on fixed models, compared with a tolerance. A shader change that makes a toon model shiny fails CI.
- **Reference assets:** Khronos glTF Sample Assets (including the unlit and extension tests), VRM samples, and FBX files exported from Blender with Lambert, Phong and PBR materials. Compared against the Khronos Sample Viewer, three-vrm and Blender.
- **Fuzzing:** `SceneBuilder` fed random meshes / indices / limits must never panic or exceed limits.
- **Benchmarks:** the §11 milestones on a fixed corpus; regressions fail CI.

---

## 16. Milestones

1. Core: `rendererx-scene` (CPU scene + builder), `Renderer` with `attach` and views, streaming upload, `clay`, on-demand rendering, camera + `controls`. Viewer shows a glTF with clay shading.
2. Shading system: registry, pipeline cache, `unlit`, `lambert`, `blinn_phong`, `pbr` + SH ambient; texture streaming.
3. Adapters: `rendererx-gltf` (zero-copy), then OBJ, STL, PLY, and FBX via `ufbx` in `viewer-fmt-3d`.
4. `mtoon` with the outline pass; environment-map lighting; KHR material extensions.
5. `pick`, mutable scene and overlays (ready for editors); `simplify`; `soft` fallback.

---

## 17. Decisions

**Math types: `glam` in the public API.**
- `Vec3`, `Mat4`, `Quat` etc. are `glam` types, re-exported as `rendererx::glam` so users never fight a version mismatch.
- glam is the de-facto standard (Bevy, wgpu examples, rend3), SIMD-fast, and `bytemuck`-compatible, so vertex data uploads without conversion. Plain arrays would mean copies and a worse API.
- An optional `mint` feature adds conversions for apps using `nalgebra`, `cgmath` or others.
- glam is pinned to one minor version per RendererX release; bumping it is a breaking change.

**Environment lighting: procedural by default, host-suppliable.**
- The default PBR environment is a **procedural studio** (soft gradient sky, a key and fill softbox, neutral floor bounce), rendered and prefiltered on the GPU the first time a PBR material needs it, as a background job: a few ms once, **0 bytes in the binary**.
- Until it is ready, SH ambient is used (§7.3), so the open path never waits for it.
- Hosts can supply their own (`Renderer::set_environment(EnvSource::Hdr | Ktx2Cubemap)`), for example a user-picked HDRI. Shipping an HDRI in the binary would cost 1–4 MB for little gain in a viewer.

**Skinning and animation: not in v1, basic playback in v2.**
- v1 shows models in their **bind / rest pose**. That is correct for most static models and keeps the first release small.
- `MeshData` reserves joint/weight and morph-target streams from day one, so v2 adds no breaking changes.
- v2 (one milestone after §16 step 5): play the file's animation clips (node TRS, skinning, morph targets) with play/pause/scrub and a clip picker. Many FBX and glTF files are characters, so a viewer is expected to play them.
- GPU skinning in the vertex shader, with joint matrices in a **texture** rather than a storage buffer, so it works on the GLES 3 backend.
- Out of scope for good: blend trees, state machines, IK, retargeting. Those belong to engines.

**Scenes are built without the GPU (decided 2026-10-31).**
- The app starts parsing at launch, before the window and device exist; requiring a `Renderer` to build a scene would serialise the two and lose ~100 ms.
- It also lets loaders run in a worker process or a WASM sandbox, and lets the `soft` CPU rasteriser draw the same scene.
- Cost: one extra crate boundary, and GPU-side state (upload progress, pipelines) is kept beside the CPU scene instead of inside it.

**Shared device, features requested up front (decided 2026-10-31).**
- RendererX never creates a device; it uses the host's, which libgui also draws with.
- The host requests every optional feature the adapter has at launch (texture compression especially), because the file type isn't known yet and a device can't gain features later. RendererX checks `device.features()` and falls back (e.g. decompress BCn to RGBA8 on the CPU) when one is missing.

**Still open:** whether RendererX moves to its own repository once its API settles (suggested: keep it in this workspace until milestone 3 is done).
