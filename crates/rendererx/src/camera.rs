use scene_data::Aabb;
use scene_data::glam::camera::rh;
use scene_data::glam::{Mat4, Vec3};

/// A perspective camera. Plain data: hosts store it per view and compare it to know
/// whether a frame is needed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub up: Vec3,
    /// Vertical field of view, radians.
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { eye: Vec3::new(0.0, 0.0, 3.0), target: Vec3::ZERO, up: Vec3::Y, fov_y: 45f32.to_radians(), near: 0.01, far: 100.0 }
    }
}

impl Camera {
    /// Looks at `bounds` from the front, a little above and to the right, at a distance
    /// where the whole box fits a view of `aspect` (width / height). Portrait views
    /// (phones) fit the narrower horizontal field of view. Reframe after a big resize.
    pub fn framing(bounds: Aabb, aspect: f32) -> Self {
        let dir = Vec3::new(0.45, 0.35, 1.0).normalize();
        Self::default().framed(bounds, dir, aspect)
    }

    /// The same camera moved along `dir` (from target to eye) to fit `bounds` in a view
    /// of `aspect` (width / height).
    pub fn framed(self, bounds: Aabb, dir: Vec3, aspect: f32) -> Self {
        if !bounds.is_valid() {
            return self;
        }
        let radius = (bounds.size().length() * 0.5).max(1e-4);
        let distance = radius / (self.narrowest_fov(aspect) * 0.5).sin().min(1.0);
        let target = bounds.center();
        Self { eye: target + dir.normalize_or(Vec3::Z) * distance, target, near: distance * 0.01, far: distance + radius * 2.0, ..self }
    }

    /// The smaller of the vertical and horizontal fields of view, radians.
    pub fn narrowest_fov(&self, aspect: f32) -> f32 {
        let fov_y = self.fov_y.clamp(1e-3, std::f32::consts::PI - 1e-3);
        let fov_x = 2.0 * ((fov_y * 0.5).tan() * aspect.max(1e-4)).atan();
        fov_y.min(fov_x)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        // wgpu's clip space: depth 0..1, Y up (glam's "directx" convention).
        let fov = self.fov_y.clamp(1e-3, std::f32::consts::PI - 1e-3);
        let near = self.near.max(1e-6);
        let proj = rh::proj::directx::perspective(fov, aspect.max(1e-4), near, self.far.max(near * 2.0));
        proj * rh::view::look_at_mat4(self.eye, self.target, self.up)
    }

    /// Towards the key light: behind the viewer, above and to the left, so it follows
    /// the camera and the visible side is always lit.
    pub(crate) fn key_light(&self) -> Vec3 {
        let back = (self.eye - self.target).normalize_or(Vec3::Z);
        let right = self.up.cross(back).normalize_or(Vec3::X);
        let up = back.cross(right);
        (back * 0.5 + up * 0.8 - right * 0.6).normalize()
    }
}

/// Orbit / pan / zoom around a target, for mouse and touch (feature `controls`).
/// Feed it input deltas; read the [`Camera`] back.
#[cfg(feature = "controls")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbitControls {
    pub target: Vec3,
    /// Around the up axis, radians.
    pub yaw: f32,
    /// Above the horizon, radians, kept short of the poles.
    pub pitch: f32,
    pub distance: f32,
    pub fov_y: f32,
    /// Scene radius, for near/far planes and zoom limits.
    pub radius: f32,
}

#[cfg(feature = "controls")]
impl OrbitControls {
    const MAX_PITCH: f32 = 1.55;

    /// See [`Camera::framing`].
    pub fn framing(bounds: Aabb, aspect: f32) -> Self {
        let cam = Camera::framing(bounds, aspect);
        let offset = cam.eye - cam.target;
        let radius = if bounds.is_valid() { (bounds.size().length() * 0.5).max(1e-4) } else { 1.0 };
        Self {
            target: cam.target,
            yaw: offset.x.atan2(offset.z),
            pitch: (offset.y / offset.length()).asin(),
            distance: offset.length(),
            fov_y: cam.fov_y,
            radius,
        }
    }

    /// Rotate by a drag of `dx`, `dy` pixels in a viewport `height` pixels tall.
    pub fn orbit(&mut self, dx: f32, dy: f32, height: f32) {
        let per_px = std::f32::consts::PI / height.max(1.0);
        self.yaw -= dx * per_px;
        self.pitch = (self.pitch + dy * per_px).clamp(-Self::MAX_PITCH, Self::MAX_PITCH);
    }

    /// Move the target so the model follows a drag of `dx`, `dy` pixels.
    pub fn pan(&mut self, dx: f32, dy: f32, height: f32) {
        let cam = self.camera();
        let back = (cam.eye - cam.target).normalize();
        let right = Vec3::Y.cross(back).normalize_or(Vec3::X);
        let up = back.cross(right);
        let world_per_px = 2.0 * self.distance * (self.fov_y * 0.5).tan() / height.max(1.0);
        self.target += (-right * dx + up * dy) * world_per_px;
    }

    /// `factor` > 1 moves closer (a pinch out, a wheel step forward).
    pub fn zoom(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.distance = (self.distance / factor).clamp(self.radius * 0.01, self.radius * 100.0);
        }
    }

    pub fn camera(&self) -> Camera {
        let dir = Vec3::new(self.pitch.cos() * self.yaw.sin(), self.pitch.sin(), self.pitch.cos() * self.yaw.cos());
        Camera {
            eye: self.target + dir * self.distance,
            target: self.target,
            up: Vec3::Y,
            fov_y: self.fov_y,
            near: (self.distance - self.radius * 2.0).max(self.distance * 0.001),
            far: self.distance + self.radius * 2.0,
        }
    }
}
