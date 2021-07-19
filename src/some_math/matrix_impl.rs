use std::{fmt::Display, ops::Mul};

use super::{Matrix3, Vector3};

impl Matrix3 {
    pub fn new(vectors: [Vector3; 3]) -> Self {
        Matrix3 { vectors }
    }
}

impl Mul<Vector3> for Matrix3 {
    type Output = Vector3;

    fn mul(self, rhs: Vector3) -> Self::Output {
        let mut res = Vector3::new([0.0, 0.0, 0.0]);
        for (vec, num) in self.vectors.iter().zip(&rhs.data) {
            res += (*vec) * (*num);
        }
        return res;
    }
}

impl Display for Matrix3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}.T, {}.T, {}.T]",
            self.vectors[0], self.vectors[1], self.vectors[2]
        )
    }
}
