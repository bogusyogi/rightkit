//! Forward real FFT ported from KISS FFT (commit febd4cae, BSD-3-Clause,
//! Copyright (c) 2003-2010 Mark Borgerding), the FFT kaldi-native-fbank 1.22.3 uses.
//!
//! Only the float, forward, power-of-two path (radix-4 and radix-2 butterflies) is
//! ported: the fbank's padded window is always a power of two. Keeping KISS FFT's
//! twiddles and operation order makes the filterbank bit-identical to Sherpa's on hosts
//! whose C build does not contract multiply-adds (x86-64 without FMA); a general-purpose
//! FFT differs by enough to move low-energy log-mel bins past 1e-4.

use super::math::Math;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Cpx {
    pub r: f32,
    pub i: f32,
}

fn mul(a: Cpx, b: Cpx) -> Cpx {
    Cpx {
        r: a.r * b.r - a.i * b.i,
        i: a.r * b.i + a.i * b.r,
    }
}

fn add(a: Cpx, b: Cpx) -> Cpx {
    Cpx {
        r: a.r + b.r,
        i: a.i + b.i,
    }
}

fn sub(a: Cpx, b: Cpx) -> Cpx {
    Cpx {
        r: a.r - b.r,
        i: a.i - b.i,
    }
}

/// `kf_cexp`: double-precision phase, stored as float.
fn cexp(phase: f64, math: Math) -> Cpx {
    Cpx {
        r: math.cos(phase) as f32,
        i: math.sin(phase) as f32,
    }
}

/// `kiss_fftr_cfg` for a forward transform of `2 * ncfft` real samples.
pub(crate) struct KissFftr {
    ncfft: usize,
    twiddles: Vec<Cpx>,
    super_twiddles: Vec<Cpx>,
    /// `(radix, stage length)` pairs from `kf_factor`.
    factors: Vec<(usize, usize)>,
}

impl KissFftr {
    /// `nfft` must be a power of two, at least 4. `math` computes the twiddles.
    pub(crate) fn new(nfft: usize, math: Math) -> Self {
        assert!(nfft >= 4 && nfft.is_power_of_two(), "kissfft port needs a power of two");
        let ncfft = nfft / 2;
        let pi = std::f64::consts::PI;
        let twiddles = (0..ncfft)
            .map(|i| cexp(-2.0 * pi * i as f64 / ncfft as f64, math))
            .collect();
        let super_twiddles = (0..ncfft / 2)
            .map(|i| cexp(-pi * ((i + 1) as f64 / ncfft as f64 + 0.5), math))
            .collect();
        // kf_factor for powers of two: fours first, then a final two.
        let mut factors = Vec::new();
        let mut n = ncfft;
        while n > 1 {
            let p = if n.is_multiple_of(4) { 4 } else { 2 };
            n /= p;
            factors.push((p, n));
        }
        Self {
            ncfft,
            twiddles,
            super_twiddles,
            factors,
        }
    }

    /// `kiss_fftr`: `input` holds `2 * ncfft` samples; `out` receives `ncfft + 1` bins.
    pub(crate) fn process(&self, input: &[f32], tmp: &mut [Cpx], out: &mut [Cpx]) {
        let n = self.ncfft;
        debug_assert!(input.len() == 2 * n && tmp.len() == n && out.len() == n + 1);
        let packed: Vec<Cpx> = input
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[r, i]| Cpx { r: *r, i: *i })
            .collect();
        self.work(tmp, &packed, 0, 1, 0);

        let tdc = tmp[0];
        out[0] = Cpx {
            r: tdc.r + tdc.i,
            i: 0.0,
        };
        out[n] = Cpx {
            r: tdc.r - tdc.i,
            i: 0.0,
        };
        for k in 1..=n / 2 {
            let fpk = tmp[k];
            let fpnk = Cpx {
                r: tmp[n - k].r,
                i: -tmp[n - k].i,
            };
            let f1k = add(fpk, fpnk);
            let f2k = sub(fpk, fpnk);
            let tw = mul(f2k, self.super_twiddles[k - 1]);
            out[k] = Cpx {
                r: (f1k.r + tw.r) * 0.5,
                i: (f1k.i + tw.i) * 0.5,
            };
            out[n - k] = Cpx {
                r: (f1k.r - tw.r) * 0.5,
                i: (tw.i - f1k.i) * 0.5,
            };
        }
    }

    /// `kf_work` with `in_stride = 1`.
    fn work(&self, fout: &mut [Cpx], fin: &[Cpx], offset: usize, fstride: usize, stage: usize) {
        let (p, m) = self.factors[stage];
        if m == 1 {
            for (j, slot) in fout[..p].iter_mut().enumerate() {
                *slot = fin[offset + j * fstride];
            }
        } else {
            for j in 0..p {
                self.work(
                    &mut fout[j * m..(j + 1) * m],
                    fin,
                    offset + j * fstride,
                    fstride * p,
                    stage + 1,
                );
            }
        }
        match p {
            4 => self.bfly4(&mut fout[..4 * m], fstride, m),
            _ => self.bfly2(&mut fout[..2 * m], fstride, m),
        }
    }

    fn bfly2(&self, fout: &mut [Cpx], fstride: usize, m: usize) {
        for k in 0..m {
            let t = mul(fout[k + m], self.twiddles[k * fstride]);
            fout[k + m] = sub(fout[k], t);
            fout[k] = add(fout[k], t);
        }
    }

    fn bfly4(&self, fout: &mut [Cpx], fstride: usize, m: usize) {
        let (m2, m3) = (2 * m, 3 * m);
        for k in 0..m {
            let s0 = mul(fout[k + m], self.twiddles[k * fstride]);
            let s1 = mul(fout[k + m2], self.twiddles[2 * k * fstride]);
            let s2 = mul(fout[k + m3], self.twiddles[3 * k * fstride]);
            let s5 = sub(fout[k], s1);
            fout[k] = add(fout[k], s1);
            let s3 = add(s0, s2);
            let s4 = sub(s0, s2);
            fout[k + m2] = sub(fout[k], s3);
            fout[k] = add(fout[k], s3);
            fout[k + m] = Cpx {
                r: s5.r + s4.i,
                i: s5.i - s4.r,
            };
            fout[k + m3] = Cpx {
                r: s5.r - s4.i,
                i: s5.i + s4.r,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Matches a direct DFT to float accuracy for every supported size path.
    #[test]
    fn matches_a_direct_dft() {
        for (nfft, math) in [8usize, 16, 64, 256, 512]
            .into_iter()
            .flat_map(|n| [(n, Math::PLATFORM), (n, Math::PORTABLE)])
        {
            let x: Vec<f32> = (0..nfft)
                .map(|i| ((i * 37 % 11) as f32 - 5.0) * 0.1 + (i as f32 * 0.3).sin())
                .collect();
            let f = KissFftr::new(nfft, math);
            let mut tmp = vec![Cpx::default(); nfft / 2];
            let mut out = vec![Cpx::default(); nfft / 2 + 1];
            f.process(&x, &mut tmp, &mut out);
            for (k, got) in out.iter().enumerate() {
                let (mut re, mut im) = (0f64, 0f64);
                for (n, v) in x.iter().enumerate() {
                    let ph = -2.0 * std::f64::consts::PI * (k * n) as f64 / nfft as f64;
                    re += *v as f64 * ph.cos();
                    im += *v as f64 * ph.sin();
                }
                assert!((got.r as f64 - re).abs() < 1e-3, "nfft {nfft} bin {k} re");
                assert!((got.i as f64 - im).abs() < 1e-3, "nfft {nfft} bin {k} im");
            }
        }
    }
}
