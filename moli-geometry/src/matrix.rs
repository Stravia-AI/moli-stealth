pub const DOM_MATRIX_COMPONENT_COUNT: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DomMatrixComponents {
    pub m11: f64,
    pub m12: f64,
    pub m13: f64,
    pub m14: f64,
    pub m21: f64,
    pub m22: f64,
    pub m23: f64,
    pub m24: f64,
    pub m31: f64,
    pub m32: f64,
    pub m33: f64,
    pub m34: f64,
    pub m41: f64,
    pub m42: f64,
    pub m43: f64,
    pub m44: f64,
}

impl DomMatrixComponents {
    pub fn identity() -> Self {
        Self {
            m11: 1.0,
            m12: 0.0,
            m13: 0.0,
            m14: 0.0,
            m21: 0.0,
            m22: 1.0,
            m23: 0.0,
            m24: 0.0,
            m31: 0.0,
            m32: 0.0,
            m33: 1.0,
            m34: 0.0,
            m41: 0.0,
            m42: 0.0,
            m43: 0.0,
            m44: 1.0,
        }
    }

    pub fn nan() -> Self {
        let nan = f64::NAN;
        Self {
            m11: nan,
            m12: nan,
            m13: nan,
            m14: nan,
            m21: nan,
            m22: nan,
            m23: nan,
            m24: nan,
            m31: nan,
            m32: nan,
            m33: nan,
            m34: nan,
            m41: nan,
            m42: nan,
            m43: nan,
            m44: nan,
        }
    }

    pub fn rotation(cos: f64, sin: f64) -> Self {
        Self {
            m11: cos,
            m12: sin,
            m21: -sin,
            m22: cos,
            ..Self::identity()
        }
    }

    pub fn rotation_axis_angle(x: f64, y: f64, z: f64, degrees: f64) -> Self {
        let length = (x * x + y * y + z * z).sqrt();
        if length == 0.0 {
            return Self::identity();
        }

        let x = x / length;
        let y = y / length;
        let z = z / length;
        let radians = degrees.to_radians();
        let cos = radians.cos();
        let sin = radians.sin();
        let one_minus_cos = 1.0 - cos;

        Self {
            m11: one_minus_cos * x * x + cos,
            m12: one_minus_cos * x * y + sin * z,
            m13: one_minus_cos * x * z - sin * y,
            m21: one_minus_cos * x * y - sin * z,
            m22: one_minus_cos * y * y + cos,
            m23: one_minus_cos * y * z + sin * x,
            m31: one_minus_cos * x * z + sin * y,
            m32: one_minus_cos * y * z - sin * x,
            m33: one_minus_cos * z * z + cos,
            ..Self::identity()
        }
    }

    pub fn perspective(distance: f64) -> Self {
        Self {
            m34: -1.0 / distance,
            ..Self::identity()
        }
    }

    pub fn translation(tx: f64, ty: f64, tz: f64) -> Self {
        Self {
            m41: tx,
            m42: ty,
            m43: tz,
            ..Self::identity()
        }
    }

    pub fn scale(sx: f64, sy: f64, sz: f64) -> Self {
        Self {
            m11: sx,
            m22: sy,
            m33: sz,
            ..Self::identity()
        }
    }

    pub fn skew_x(tan: f64) -> Self {
        Self {
            m21: tan,
            ..Self::identity()
        }
    }

    pub fn skew_y(tan: f64) -> Self {
        Self {
            m12: tan,
            ..Self::identity()
        }
    }

    pub fn multiply(self, other: Self) -> Self {
        Self {
            m11: self.m11 * other.m11
                + self.m21 * other.m12
                + self.m31 * other.m13
                + self.m41 * other.m14,
            m12: self.m12 * other.m11
                + self.m22 * other.m12
                + self.m32 * other.m13
                + self.m42 * other.m14,
            m13: self.m13 * other.m11
                + self.m23 * other.m12
                + self.m33 * other.m13
                + self.m43 * other.m14,
            m14: self.m14 * other.m11
                + self.m24 * other.m12
                + self.m34 * other.m13
                + self.m44 * other.m14,
            m21: self.m11 * other.m21
                + self.m21 * other.m22
                + self.m31 * other.m23
                + self.m41 * other.m24,
            m22: self.m12 * other.m21
                + self.m22 * other.m22
                + self.m32 * other.m23
                + self.m42 * other.m24,
            m23: self.m13 * other.m21
                + self.m23 * other.m22
                + self.m33 * other.m23
                + self.m43 * other.m24,
            m24: self.m14 * other.m21
                + self.m24 * other.m22
                + self.m34 * other.m23
                + self.m44 * other.m24,
            m31: self.m11 * other.m31
                + self.m21 * other.m32
                + self.m31 * other.m33
                + self.m41 * other.m34,
            m32: self.m12 * other.m31
                + self.m22 * other.m32
                + self.m32 * other.m33
                + self.m42 * other.m34,
            m33: self.m13 * other.m31
                + self.m23 * other.m32
                + self.m33 * other.m33
                + self.m43 * other.m34,
            m34: self.m14 * other.m31
                + self.m24 * other.m32
                + self.m34 * other.m33
                + self.m44 * other.m34,
            m41: self.m11 * other.m41
                + self.m21 * other.m42
                + self.m31 * other.m43
                + self.m41 * other.m44,
            m42: self.m12 * other.m41
                + self.m22 * other.m42
                + self.m32 * other.m43
                + self.m42 * other.m44,
            m43: self.m13 * other.m41
                + self.m23 * other.m42
                + self.m33 * other.m43
                + self.m43 * other.m44,
            m44: self.m14 * other.m41
                + self.m24 * other.m42
                + self.m34 * other.m43
                + self.m44 * other.m44,
        }
    }

    pub fn translated(mut self, tx: f64, ty: f64, tz: f64) -> Self {
        self.m41 += self.m11 * tx + self.m21 * ty + self.m31 * tz;
        self.m42 += self.m12 * tx + self.m22 * ty + self.m32 * tz;
        self.m43 += self.m13 * tx + self.m23 * ty + self.m33 * tz;
        self.m44 += self.m14 * tx + self.m24 * ty + self.m34 * tz;
        self
    }

    pub fn scaled_2d(mut self, scale_x: f64, scale_y: f64) -> Self {
        self.m11 *= scale_x;
        self.m12 *= scale_x;
        self.m13 *= scale_x;
        self.m14 *= scale_x;
        self.m21 *= scale_y;
        self.m22 *= scale_y;
        self.m23 *= scale_y;
        self.m24 *= scale_y;
        self
    }

    pub fn scaled_3d(mut self, scale_x: f64, scale_y: f64, scale_z: f64) -> Self {
        self.m11 *= scale_x;
        self.m12 *= scale_x;
        self.m13 *= scale_x;
        self.m14 *= scale_x;
        self.m21 *= scale_y;
        self.m22 *= scale_y;
        self.m23 *= scale_y;
        self.m24 *= scale_y;
        self.m31 *= scale_z;
        self.m32 *= scale_z;
        self.m33 *= scale_z;
        self.m34 *= scale_z;
        self
    }

    pub fn scaled_with_origin(
        mut self,
        scale_x: f64,
        scale_y: f64,
        scale_z: f64,
        origin_x: f64,
        origin_y: f64,
        origin_z: f64,
    ) -> Self {
        let has_origin = origin_x != 0.0 || origin_y != 0.0 || origin_z != 0.0;
        if has_origin {
            self = self.translated(origin_x, origin_y, origin_z);
        }
        self = self.scaled_3d(scale_x, scale_y, scale_z);
        if has_origin {
            self = self.translated(-origin_x, -origin_y, -origin_z);
        }
        self
    }

    pub fn rotated(mut self, rot_x: f64, rot_y: f64, rot_z: f64) -> Self {
        if rot_z != 0.0 {
            self = self.rotated_z(rot_z);
        }
        if rot_y != 0.0 {
            self = self.rotated_axis_angle(0.0, 1.0, 0.0, rot_y);
        }
        if rot_x != 0.0 {
            self = self.rotated_axis_angle(1.0, 0.0, 0.0, rot_x);
        }
        self
    }

    pub fn rotated_z(self, degrees: f64) -> Self {
        let radians = degrees.to_radians();
        self.multiply(Self::rotation(radians.cos(), radians.sin()))
    }

    pub fn rotated_axis_angle(self, x: f64, y: f64, z: f64, degrees: f64) -> Self {
        let length = (x * x + y * y + z * z).sqrt();
        if length == 0.0 {
            return self;
        }
        self.multiply(Self::rotation_axis_angle(x, y, z, degrees))
    }

    pub fn skewed_x(self, degrees: f64) -> Self {
        self.multiply(Self::skew_x(degrees.to_radians().tan()))
    }

    pub fn skewed_y(self, degrees: f64) -> Self {
        self.multiply(Self::skew_y(degrees.to_radians().tan()))
    }

    pub fn inverse(self) -> Self {
        let mut matrix = [
            [self.m11, self.m21, self.m31, self.m41],
            [self.m12, self.m22, self.m32, self.m42],
            [self.m13, self.m23, self.m33, self.m43],
            [self.m14, self.m24, self.m34, self.m44],
        ];
        let mut inverse = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];

        for column in 0..4 {
            let Some(pivot_row) = (column..4).max_by(|left, right| {
                matrix[*left][column]
                    .abs()
                    .total_cmp(&matrix[*right][column].abs())
            }) else {
                return Self::nan();
            };
            let pivot = matrix[pivot_row][column];
            if pivot == 0.0 || !pivot.is_finite() {
                return Self::nan();
            }

            matrix.swap(column, pivot_row);
            inverse.swap(column, pivot_row);

            for index in 0..4 {
                matrix[column][index] /= pivot;
                inverse[column][index] /= pivot;
            }

            for row in 0..4 {
                if row == column {
                    continue;
                }
                let factor = matrix[row][column];
                for index in 0..4 {
                    matrix[row][index] -= factor * matrix[column][index];
                    inverse[row][index] -= factor * inverse[column][index];
                }
            }
        }

        if inverse.iter().flatten().any(|value| !value.is_finite()) {
            return Self::nan();
        }

        Self {
            m11: inverse[0][0],
            m21: inverse[0][1],
            m31: inverse[0][2],
            m41: inverse[0][3],
            m12: inverse[1][0],
            m22: inverse[1][1],
            m32: inverse[1][2],
            m42: inverse[1][3],
            m13: inverse[2][0],
            m23: inverse[2][1],
            m33: inverse[2][2],
            m43: inverse[2][3],
            m14: inverse[3][0],
            m24: inverse[3][1],
            m34: inverse[3][2],
            m44: inverse[3][3],
        }
    }

    pub fn is_2d(self) -> bool {
        self.m13 == 0.0
            && self.m14 == 0.0
            && self.m23 == 0.0
            && self.m24 == 0.0
            && self.m31 == 0.0
            && self.m32 == 0.0
            && self.m33 == 1.0
            && self.m34 == 0.0
            && self.m43 == 0.0
            && self.m44 == 1.0
    }

    pub fn is_identity(self) -> bool {
        self == Self::identity()
    }

    pub fn css_text(self) -> Option<String> {
        if self.is_2d() {
            let values = [self.m11, self.m12, self.m21, self.m22, self.m41, self.m42];
            if !values.iter().all(|value| value.is_finite()) {
                return None;
            }
            return Some(format!(
                "matrix({}, {}, {}, {}, {}, {})",
                css_number(self.m11),
                css_number(self.m12),
                css_number(self.m21),
                css_number(self.m22),
                css_number(self.m41),
                css_number(self.m42)
            ));
        }

        let values = [
            self.m11, self.m12, self.m13, self.m14, self.m21, self.m22, self.m23, self.m24,
            self.m31, self.m32, self.m33, self.m34, self.m41, self.m42, self.m43, self.m44,
        ];
        if !values.iter().all(|value| value.is_finite()) {
            return None;
        }
        Some(format!(
            "matrix3d({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {})",
            css_number(self.m11),
            css_number(self.m12),
            css_number(self.m13),
            css_number(self.m14),
            css_number(self.m21),
            css_number(self.m22),
            css_number(self.m23),
            css_number(self.m24),
            css_number(self.m31),
            css_number(self.m32),
            css_number(self.m33),
            css_number(self.m34),
            css_number(self.m41),
            css_number(self.m42),
            css_number(self.m43),
            css_number(self.m44)
        ))
    }
}

fn css_number(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else if value.abs() < 0.000001 {
        trim_fraction_trailing_zeros(format!("{value:.22}"))
    } else {
        value.to_string()
    }
}

fn trim_fraction_trailing_zeros(mut serialized: String) -> String {
    if serialized.contains('.') {
        while serialized.ends_with('0') {
            serialized.pop();
        }
        if serialized.ends_with('.') {
            serialized.pop();
        }
    }
    serialized
}
