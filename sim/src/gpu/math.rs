//! The little matrix maths the viewports need. Hand-rolled to keep the demo's
//! dependency list honest — a 4x4 multiply is not worth a crate.
//!
//! Column-vector convention, row-major storage, uploaded transposed for HLSL's
//! default column-major packing.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4(pub [[f32; 4]; 4]);

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);

    #[allow(clippy::should_implement_trait)] // `Mat4 * Mat4` reads worse here.
    pub fn mul(self, rhs: Mat4) -> Mat4 {
        let mut out = [[0.0f32; 4]; 4];
        for (r, row) in out.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                *cell = (0..4).map(|k| self.0[r][k] * rhs.0[k][c]).sum();
            }
        }
        Mat4(out)
    }

    /// Row-major → column-major, which is what HLSL expects by default.
    pub fn transposed(self) -> [f32; 16] {
        let mut out = [0.0f32; 16];
        for r in 0..4 {
            for c in 0..4 {
                out[c * 4 + r] = self.0[r][c];
            }
        }
        out
    }

    /// Left-handed perspective with a [0,1] depth range — the D3D convention.
    /// The Vulkan viewport flips Y at the viewport rather than here, so both
    /// APIs consume the identical matrix and cannot disagree about the scene.
    pub fn perspective_lh(fov_y: f32, aspect: f32, znear: f32, zfar: f32) -> Mat4 {
        let h = 1.0 / (fov_y * 0.5).tan();
        let w = h / aspect.max(1e-6);
        let range = zfar / (zfar - znear);
        Mat4([
            [w, 0.0, 0.0, 0.0],
            [0.0, h, 0.0, 0.0],
            [0.0, 0.0, range, -range * znear],
            [0.0, 0.0, 1.0, 0.0],
        ])
    }

    /// The orthonormal basis a look-at view is built from: `(right, up, forward)`.
    /// The billboard code needs these, and deriving them twice invites the two
    /// copies to disagree.
    pub fn look_at_basis(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let f = normalize(sub(target, eye));
        let s = normalize(cross(up, f));
        let u = cross(f, s);
        (s, u, f)
    }

    pub fn look_at_lh(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> Mat4 {
        let f = normalize(sub(target, eye));
        let s = normalize(cross(up, f));
        let u = cross(f, s);
        Mat4([
            [s[0], s[1], s[2], -dot(s, eye)],
            [u[0], u[1], u[2], -dot(u, eye)],
            [f[0], f[1], f[2], -dot(f, eye)],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    pub fn translation(t: [f32; 3]) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[0][3] = t[0];
        m.0[1][3] = t[1];
        m.0[2][3] = t[2];
        m
    }

    pub fn scale(s: f32) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[0][0] = s;
        m.0[1][1] = s;
        m.0[2][2] = s;
        m
    }
}

pub fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt().max(1e-6);
    [v[0] / len, v[1] / len, v[2] / len]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_neutral() {
        let m = Mat4::perspective_lh(1.0, 1.6, 0.1, 100.0);
        assert_eq!(m.mul(Mat4::IDENTITY), m);
        assert_eq!(Mat4::IDENTITY.mul(m), m);
    }

    #[test]
    fn look_at_puts_the_target_on_the_forward_axis() {
        let m = Mat4::look_at_lh([0.0, 0.0, -10.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        // The target should land on +Z in view space, at the eye distance.
        let t = [0.0, 0.0, 0.0, 1.0];
        let z: f32 = (0..4).map(|k| m.0[2][k] * t[k]).sum();
        assert!((z - 10.0).abs() < 1e-4, "{z}");
    }
}
