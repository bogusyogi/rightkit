//! The transcendental functions of the filterbank (per-frame log, window, FFT twiddles) and
//! the joiner softmax.
//!
//! By default they are the platform's (`f32::ln`, `f64::cos`, ...), which resolve to the C
//! math library each OS ships, as Sherpa's do; their last-ulp results differ between glibc,
//! the macOS libm and the MSVC CRT. `Math::new(true)` uses the pure-Rust `libm` crate
//! instead, so these stages compute the same bits on every OS (ONNX Runtime's own kernels
//! are outside its reach).

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Math {
    portable: bool,
}

impl Math {
    #[cfg(test)]
    pub(crate) const PLATFORM: Self = Self { portable: false };
    #[cfg(test)]
    pub(crate) const PORTABLE: Self = Self { portable: true };

    pub(crate) fn new(portable: bool) -> Self {
        Self { portable }
    }

    pub(crate) fn expf(self, x: f32) -> f32 {
        if self.portable {
            libm::expf(x)
        } else {
            x.exp()
        }
    }

    pub(crate) fn lnf(self, x: f32) -> f32 {
        if self.portable {
            libm::logf(x)
        } else {
            x.ln()
        }
    }

    pub(crate) fn exp(self, x: f64) -> f64 {
        if self.portable {
            libm::exp(x)
        } else {
            x.exp()
        }
    }

    pub(crate) fn ln(self, x: f64) -> f64 {
        if self.portable {
            libm::log(x)
        } else {
            x.ln()
        }
    }

    pub(crate) fn ln_1p(self, x: f64) -> f64 {
        if self.portable {
            libm::log1p(x)
        } else {
            x.ln_1p()
        }
    }

    pub(crate) fn cos(self, x: f64) -> f64 {
        if self.portable {
            libm::cos(x)
        } else {
            x.cos()
        }
    }

    pub(crate) fn sin(self, x: f64) -> f64 {
        if self.portable {
            libm::sin(x)
        } else {
            x.sin()
        }
    }

    pub(crate) fn powf(self, x: f64, y: f64) -> f64 {
        if self.portable {
            libm::pow(x, y)
        } else {
            x.powf(y)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Units in the last place between two finite floats of one sign.
    fn ulps32(a: f32, b: f32) -> u32 {
        (a.to_bits() as i64 - b.to_bits() as i64).unsigned_abs() as u32
    }

    fn ulps64(a: f64, b: f64) -> u64 {
        (a.to_bits() as i128 - b.to_bits() as i128).unsigned_abs() as u64
    }

    #[test]
    fn portable_math_stays_within_two_ulps_of_the_platform() {
        let (p, q) = (Math::PLATFORM, Math::PORTABLE);
        for i in 1..2_000 {
            let x = i as f32 * 0.013 - 13.0;
            assert!(ulps32(p.expf(x), q.expf(x)) <= 2, "expf({x})");
            let y = i as f32 * 0.37;
            assert!(ulps32(p.lnf(y), q.lnf(y)) <= 2, "logf({y})");
            let z = i as f64 * 0.0031;
            assert!(ulps64(p.exp(-z), q.exp(-z)) <= 2, "exp({z})");
            assert!(ulps64(p.ln(z), q.ln(z)) <= 2, "log({z})");
            assert!(ulps64(p.ln_1p(z), q.ln_1p(z)) <= 2, "log1p({z})");
            assert!(ulps64(p.cos(z), q.cos(z)) <= 2, "cos({z})");
            assert!(ulps64(p.sin(z), q.sin(z)) <= 2, "sin({z})");
            let w = (i as f64 / 2_000.0).max(1e-3);
            assert!(
                ulps64(p.powf(w, 0.85), q.powf(w, 0.85)) <= 2,
                "pow({w}, 0.85)"
            );
        }
        // The fbank floor and the softmax extremes.
        assert_eq!(q.lnf(f32::EPSILON), f32::EPSILON.ln());
        assert_eq!(q.expf(0.0), 1.0);
        assert_eq!(q.expf(-200.0), 0.0);
    }
}
