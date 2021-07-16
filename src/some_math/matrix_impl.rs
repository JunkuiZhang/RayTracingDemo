use std::ops::Mul;

use super::{Matrix3, Vector3};

impl Matrix3 {
    pub fn new(vectors: [Vector3; 3]) -> Self {
        Matrix3 { vectors }
    }
}

impl Mul<Vector3> for Matrix3 {
    type Output = Vector3;

    fn mul(self, rhs: Vector3) -> Self::Output {
        self.vectors[0] * rhs.data[0]
            + self.vectors[1] * rhs.data[1]
            + self.vectors[2] * rhs.data[2]
    }
}
