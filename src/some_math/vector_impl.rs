use std::{
    fmt::Display,
    ops::{Add, AddAssign, Div, DivAssign, Mul, Sub},
    usize,
};

use super::{clamp, Color, Vector3};

impl Vector3 {
    pub const BLACK: Color = Color { data: [0.0; 3] };

    pub fn new(data: [f64; 3]) -> Self {
        Vector3 { data }
    }

    pub fn unit_vec_from_axis(axis: usize) -> Option<Self> {
        match axis {
            0 => {
                return Some(Vector3::new([1.0, 0.0, 0.0]));
            }
            1 => {
                return Some(Vector3::new([0.0, 1.0, 0.0]));
            }
            2 => {
                return Some(Vector3::new([0.0, 0.0, 1.0]));
            }
            _ => return None,
        }
    }

    pub fn x(&self) -> f64 {
        self.data[0]
    }

    pub fn y(&self) -> f64 {
        self.data[1]
    }

    pub fn z(&self) -> f64 {
        self.data[2]
    }

    pub fn to_u8(&self) -> [u8; 3] {
        let mut res = [0; 3];
        for (num, r) in self.data.iter().zip(&mut res) {
            *r = (clamp((*num).sqrt(), 0.0, 1.0) * 255.0) as u8;
        }
        return res;
    }

    pub fn length_square(&self) -> f64 {
        return self.data.iter().map(|num| (*num) * (*num)).sum();
    }

    pub fn length(&self) -> f64 {
        self.length_square().sqrt()
    }

    pub fn normalize(&self) -> Vector3 {
        *self / self.length()
    }

    pub fn naive_mul(&self, rhs: Vector3) -> Vector3 {
        let mut data = [0.0; 3];
        for ((a, b), r) in self.data.iter().zip(&rhs.data).zip(&mut data) {
            *r = (*a) * (*b);
        }
        return Vector3 { data };
    }

    pub fn get_axis(&self) -> usize {
        let mut n = 0;
        if self.data[1].abs() > 1e-3 {
            n = 1;
        } else if self.data[2].abs() > 1e-3 {
            n = 2;
        }
        return n;
    }

    pub fn cross_product(&self, rhs: Vector3) -> Vector3 {
        Vector3 {
            data: [
                self.data[1] * rhs.data[2] - self.data[2] * rhs.data[1],
                self.data[2] * rhs.data[0] - self.data[0] * rhs.data[2],
                self.data[0] * rhs.data[1] - self.data[1] * rhs.data[0],
            ],
        }
    }
}

impl Add<Vector3> for Vector3 {
    type Output = Vector3;

    fn add(self, rhs: Vector3) -> Self::Output {
        let mut data = [0.0; 3];
        for ((s, r), res) in self.data.iter().zip(&rhs.data).zip(&mut data) {
            *res = *s + *r;
        }
        return Vector3 { data };
    }
}

impl AddAssign<Vector3> for Vector3 {
    fn add_assign(&mut self, rhs: Vector3) {
        for (a, b) in self.data.iter_mut().zip(&rhs.data) {
            *a += *b;
        }
    }
}

impl Mul<Vector3> for Vector3 {
    type Output = f64;

    fn mul(self, rhs: Vector3) -> Self::Output {
        let mut res = 0.0;
        for (a, b) in self.data.iter().zip(&rhs.data) {
            res += (*a) * (*b);
        }
        return res;
    }
}

impl Mul<f64> for Vector3 {
    type Output = Vector3;

    fn mul(self, rhs: f64) -> Self::Output {
        let mut data = [0.0; 3];
        for (num, res) in self.data.iter().zip(&mut data) {
            *res = (*num) * rhs;
        }
        return Vector3 { data };
    }
}

impl Mul<Vector3> for f64 {
    type Output = Vector3;

    fn mul(self, rhs: Vector3) -> Self::Output {
        rhs * self
    }
}

impl Div<f64> for Vector3 {
    type Output = Vector3;

    fn div(self, rhs: f64) -> Self::Output {
        let mut data = [0.0; 3];
        for (num, res) in self.data.iter().zip(&mut data) {
            *res = (*num) / rhs;
        }
        return Vector3 { data };
    }
}

impl DivAssign<f64> for Vector3 {
    fn div_assign(&mut self, rhs: f64) {
        for num in self.data.iter_mut() {
            *num /= rhs;
        }
    }
}

impl Sub<Vector3> for Vector3 {
    type Output = Vector3;

    fn sub(self, rhs: Vector3) -> Self::Output {
        let mut data = [0.0; 3];
        for ((a, b), r) in self.data.iter().zip(&rhs.data).zip(&mut data) {
            *r = *a - *b;
        }
        return Vector3 { data };
    }
}

impl Display for Vector3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {}, {})", self.data[0], self.data[1], self.data[2])
    }
}
