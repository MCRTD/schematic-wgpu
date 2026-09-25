//! Orbit camera: rotates around a target point, Y up.

use glam::{Mat4, Vec3};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbitCamera {
    pub target: Vec3,
    /// Horizontal angle in radians; 0 looks from +Z (south) towards north.
    pub yaw: f32,
    /// Vertical angle in radians; positive looks down from above.
    pub pitch: f32,
    pub distance: f32,
    /// Vertical field of view in radians.
    pub fov_y: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            yaw: 45f32.to_radians(),
            pitch: 30f32.to_radians(),
            distance: 10.0,
            fov_y: 45f32.to_radians(),
        }
    }
}

impl OrbitCamera {
    const MAX_PITCH: f32 = 89.0 * std::f32::consts::PI / 180.0;

    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        self.target + Vec3::new(cp * sy, sp, cp * cy) * self.distance
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        // Keep depth precision reasonable for both close-ups and large builds.
        let near = (self.distance * 0.005).max(0.01);
        let far = self.distance * 10.0 + 100.0;
        // wgpu clip space matches DirectX: depth 0..1, Y up.
        let proj = glam::camera::rh::proj::directx::perspective(self.fov_y, aspect, near, far);
        proj * glam::camera::rh::view::look_at_mat4(self.eye(), self.target, Vec3::Y)
    }

    /// Point the camera at an axis-aligned box so all of it is visible.
    pub fn frame(&mut self, min: Vec3, max: Vec3) {
        self.target = (min + max) / 2.0;
        let radius = ((max - min).length() / 2.0).max(0.5);
        self.distance = radius / (self.fov_y / 2.0).sin() * 1.05;
    }

    /// Rotate by a mouse drag, in pixels.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.008;
        self.pitch = (self.pitch + dy * 0.008).clamp(-Self::MAX_PITCH, Self::MAX_PITCH);
    }

    /// Move the target in the view plane so the scene follows the cursor.
    pub fn pan(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let forward = (self.target - self.eye()).normalize();
        let right = forward.cross(Vec3::Y).normalize();
        let up = right.cross(forward);
        // World units per pixel at the target's depth.
        let scale = 2.0 * self.distance * (self.fov_y / 2.0).tan() / viewport_height.max(1.0);
        self.target += (-right * dx + up * dy) * scale;
    }

    /// Zoom by scroll steps; positive moves closer.
    pub fn zoom(&mut self, steps: f32) {
        self.distance = (self.distance * 0.9f32.powf(steps)).clamp(0.5, 100_000.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framed_box_is_inside_the_frustum() {
        let mut cam = OrbitCamera::default();
        let (min, max) = (Vec3::new(3.0, 0.0, 1.0), Vec3::new(137.0, 5.0, 11.0));
        cam.frame(min, max);
        let vp = cam.view_proj(16.0 / 9.0);
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { min.x } else { max.x },
                if i & 2 == 0 { min.y } else { max.y },
                if i & 4 == 0 { min.z } else { max.z },
            );
            let clip = vp.project_point3(corner);
            assert!(clip.x.abs() <= 1.0 && clip.y.abs() <= 1.0 && (0.0..=1.0).contains(&clip.z), "{corner} -> {clip}");
        }
    }

    #[test]
    fn pan_moves_target_by_screen_distance() {
        let mut cam = OrbitCamera { distance: 10.0, ..Default::default() };
        let eye_to_target = cam.target - cam.eye();
        cam.pan(0.0, 100.0, 800.0);
        assert!(cam.target.dot(eye_to_target).abs() < 1e-4);
        let expected = 2.0 * 10.0 * (cam.fov_y / 2.0).tan() * 100.0 / 800.0;
        assert!((cam.target.length() - expected).abs() < 1e-4);
    }

    #[test]
    fn pitch_is_clamped_and_zoom_is_bounded() {
        let mut cam = OrbitCamera::default();
        cam.orbit(0.0, 1e6);
        assert!(cam.pitch <= OrbitCamera::MAX_PITCH);
        cam.zoom(1e4);
        assert_eq!(cam.distance, 0.5);
    }
}
