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
