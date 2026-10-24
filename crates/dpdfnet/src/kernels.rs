//! FP32 inference primitives for the fixed DPDFNet graph. Safe Rust only.
//! Algorithm/layout ported from CEVA's Apache-2.0 native inference reference.

fn disjoint(
    data: &mut [f32],
    src: usize,
    count: usize,
    dst: usize,
    output: usize,
) -> (&[f32], &mut [f32]) {
    assert!(
        src + count <= dst || dst + output <= src,
        "overlapping neural tensor operands"
    );
    if src < dst {
        let (left, right) = data.split_at_mut(dst);
        (&left[src..src + count], &mut right[..output])
    } else {
        let (left, right) = data.split_at_mut(src);
        (&right[..count], &mut left[dst..dst + output])
    }
}

pub(super) fn transpose(data: &mut [f32], src: usize, dst: usize, rows: usize, cols: usize) {
    let (x, y) = disjoint(data, src, rows * cols, dst, rows * cols);
    for r in (0..rows).step_by(8) {
        for c in (0..cols).step_by(8) {
            for i in r..(r + 8).min(rows) {
                for j in c..(c + 8).min(cols) {
                    y[j * rows + i] = x[i * cols + j];
                }
            }
        }
    }
}

fn affine(x: &[f32], w: &[f32], bias: &[f32], y: &mut [f32], rows: usize, k: usize, n: usize) {
    // Reuse each weight vector across four frequency rows and retain small
    // accumulators in registers. Reduction order stays identical to scalar FP32.
    let tiled_rows = rows / 4 * 4;
    let tiled_cols = n / 8 * 8;
    for r in (0..tiled_rows).step_by(4) {
        for c in (0..tiled_cols).step_by(8) {
            let mut a: [f32; 8] = bias[c..c + 8].try_into().unwrap();
            let mut b = a;
            let mut d = a;
            let mut e = a;
            for j in 0..k {
                let row = &w[j * n + c..j * n + c + 8];
                let x0 = x[r * k + j];
                let x1 = x[(r + 1) * k + j];
                let x2 = x[(r + 2) * k + j];
                let x3 = x[(r + 3) * k + j];
                for i in 0..8 {
                    a[i] += x0 * row[i];
                    b[i] += x1 * row[i];
                    d[i] += x2 * row[i];
                    e[i] += x3 * row[i];
                }
            }
            y[r * n + c..r * n + c + 8].copy_from_slice(&a);
            y[(r + 1) * n + c..(r + 1) * n + c + 8].copy_from_slice(&b);
            y[(r + 2) * n + c..(r + 2) * n + c + 8].copy_from_slice(&d);
            y[(r + 3) * n + c..(r + 3) * n + c + 8].copy_from_slice(&e);
        }
    }
    for r in 0..rows {
        let first = if r < tiled_rows { tiled_cols } else { 0 };
        if first == n {
            continue;
        }
        let out = &mut y[r * n + first..(r + 1) * n];
        out.copy_from_slice(&bias[first..n]);
        for j in 0..k {
            let value = x[r * k + j];
            let row = &w[j * n + first..(j + 1) * n];
            for (out, weight) in out.iter_mut().zip(row) {
                *out += value * weight;
            }
        }
    }
}

pub(super) struct Dense {
    k: usize,
    n: usize,
    w: Vec<f32>,
    bias: Vec<f32>,
}
impl Dense {
    pub fn new(
        weights: &[f32],
        offset: usize,
        bias: Option<usize>,
        k: usize,
        n: usize,
        tile: usize,
    ) -> Self {
        let mut w = vec![0.; k * n];
        for j in 0..k {
            for c in 0..n {
                w[j * n + c] = weights[offset
                    + if tile == 0 {
                        j * n + c
                    } else {
                        (c / 64) * k * 64 + j * 64 + c % 64
                    }];
            }
        }
        Self {
            k,
            n,
            w,
            bias: bias.map_or_else(|| vec![0.; n], |b| weights[b..b + n].to_vec()),
        }
    }
    pub fn run(&self, data: &mut [f32], src: usize, dst: usize, rows: usize) {
        let (x, y) = disjoint(data, src, rows * self.k, dst, rows * self.n);
        self.run_state(x, y, rows);
    }
    pub fn run_state(&self, x: &[f32], y: &mut [f32], rows: usize) {
        affine(x, &self.w, &self.bias, y, rows, self.k, self.n);
    }
}

pub(super) struct Conv {
    shape: [usize; 13],
    w: Vec<f32>,
    bias: Vec<f32>,
}
impl Conv {
    pub fn new(weights: &[f32], offset: usize, bias: Option<usize>, shape: [usize; 13]) -> Self {
        let [ci, _, _, co, _, _, kh, kw, _, _, _, _, groups] = shape;
        Self {
            shape,
            w: weights[offset..offset + co * (ci / groups) * kh * kw].to_vec(),
            bias: bias.map_or_else(|| vec![0.; co], |b| weights[b..b + co].to_vec()),
        }
    }
    pub fn run(&self, data: &mut [f32], src: usize, dst: usize) {
        let [ci, hi, wi, co, ho, wo, kh, kw, sh, sw, ph, pw, groups] = self.shape;
        let (x, y) = disjoint(data, src, ci * hi * wi, dst, co * ho * wo);
        let cig = ci / groups;
        let cog = co / groups;
        for oc in 0..co {
            let out = &mut y[oc * ho * wo..(oc + 1) * ho * wo];
            out.fill(self.bias[oc]);
            for ic in 0..cig {
                for ky in 0..kh {
                    for kx in 0..kw {
                        let weight = self.w[((oc * cig + ic) * kh + ky) * kw + kx];
                        for oh in 0..ho {
                            let ih = (oh * sh + ky) as isize - ph as isize;
                            if ih < 0 || ih >= hi as isize {
                                continue;
                            }
                            let input = &x[((oc / cog * cig + ic) * hi + ih as usize) * wi..][..wi];
                            let begin = pw.saturating_sub(kx).div_ceil(sw).min(wo);
                            let end = ((wi + pw).saturating_sub(kx)).div_ceil(sw).min(wo);
                            if begin >= end {
                                continue;
                            }
                            if sw == 1 {
                                let row = &input[begin + kx - pw..end + kx - pw];
                                for (output, value) in
                                    out[oh * wo + begin..oh * wo + end].iter_mut().zip(row)
                                {
                                    *output += weight * value;
                                }
                            } else {
                                for ow in begin..end {
                                    out[oh * wo + ow] += weight * input[ow * sw + kx - pw];
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn gates(a: &[f32], b: &[f32], old: &[f32], out: &mut [f32], rows: usize) {
    for r in 0..rows {
        for c in 0..64 {
            let reset = 1. / (1. + (-(a[r * 192 + c] + b[r * 192 + c])).exp());
            let update = 1. / (1. + (-(a[r * 192 + 64 + c] + b[r * 192 + 64 + c])).exp());
            let candidate = (a[r * 192 + 128 + c] + reset * b[r * 192 + 128 + c]).tanh();
            out[r * 64 + c] = candidate + update * (old[r * 64 + c] - candidate);
        }
    }
}

fn norm_residual(
    out: &mut [f32],
    p: &[f32],
    skip: &[f32],
    scale: &[f32],
    bias: &[f32],
    epsilon: f32,
    rows: usize,
) {
    for r in 0..rows {
        let row = &p[r * 64..(r + 1) * 64];
        let mean = row.iter().map(|v| f64::from(*v)).sum::<f64>() / 64.;
        let variance = row
            .iter()
            .map(|v| (f64::from(*v) - mean).powi(2))
            .sum::<f64>()
            / 64.;
        let inverse = (1. / (variance + f64::from(epsilon)).sqrt()) as f32;
        for c in 0..64 {
            out[r * 64 + c] =
                ((row[c] - mean as f32) * inverse) * scale[c] + bias[c] + skip[r * 64 + c];
        }
    }
}

pub(super) struct Block {
    f: usize,
    w: Vec<f32>,
    epsilon: [f32; 2],
    x: Vec<f32>,
    bi: Vec<f32>,
    mid: Vec<f32>,
    a: Vec<f32>,
    hidden: Vec<f32>,
    projected: Vec<f32>,
    result: Vec<f32>,
    output: Vec<f32>,
    state: Vec<f32>,
}
impl Block {
    pub fn new(weights: &[f32], f: usize, offset: usize, e0: f32, e1: f32) -> Self {
        Self {
            f,
            w: weights[offset..offset + 87552].to_vec(),
            epsilon: [e0, e1],
            x: vec![0.; f * 64],
            bi: vec![0.; f * 128],
            mid: vec![0.; f * 64],
            a: vec![0.; f * 192],
            hidden: vec![0.; f * 192],
            projected: vec![0.; f * 64],
            result: vec![0.; f * 64],
            output: vec![0.; f * 64],
            state: vec![0.; f * 64],
        }
    }
    pub fn size(&self) -> usize {
        self.f * 64
    }
    pub fn output(&self) -> &[f32] {
        &self.output
    }
    pub fn state_out(&self) -> &[f32] {
        &self.state
    }
    pub fn run(&mut self, input: &[f32], state: &[f32], layout: usize) {
        let f = self.f;
        let w = &self.w;
        if layout & 1 != 0 {
            self.x.copy_from_slice(input);
        } else {
            for r in 0..f {
                for c in 0..64 {
                    self.x[r * 64 + c] = input[c * f + r];
                }
            }
        }
        for direction in 0..2 {
            let intra = if direction == 0 {
                &mut self.a
            } else {
                &mut self.hidden
            };
            affine(
                &self.x,
                &w[direction * 12288..],
                &w[49152 + direction * 384..],
                intra,
                f,
                64,
                192,
            );
            let mut h = [0.; 64];
            let mut step = [0.; 192];
            for t in 0..f {
                let r = if direction == 0 { t } else { f - 1 - t };
                affine(
                    &h,
                    &w[24576 + direction * 12288..],
                    &w[49152 + direction * 384 + 192..],
                    &mut step,
                    1,
                    64,
                    192,
                );
                let previous = h;
                gates(&intra[r * 192..], &step, &previous, &mut h, 1);
                self.bi[r * 128 + direction * 64..r * 128 + (direction + 1) * 64]
                    .copy_from_slice(&h);
            }
        }
        affine(
            &self.bi,
            &w[49920..],
            &w[58112..],
            &mut self.projected,
            f,
            128,
            64,
        );
        norm_residual(
            &mut self.mid,
            &self.projected,
            &self.x,
            &w[58176..],
            &w[58240..],
            self.epsilon[0],
            f,
        );
        affine(&self.mid, &w[58304..], &w[82880..], &mut self.a, f, 64, 192);
        affine(
            state,
            &w[70592..],
            &w[83072..],
            &mut self.hidden,
            f,
            64,
            192,
        );
        gates(&self.a, &self.hidden, state, &mut self.state, f);
        affine(
            &self.state,
            &w[83264..],
            &w[87360..],
            &mut self.projected,
            f,
            64,
            64,
        );
        norm_residual(
            &mut self.result,
            &self.projected,
            &self.mid,
            &w[87424..],
            &w[87488..],
            self.epsilon[1],
            f,
        );
        if layout & 2 != 0 {
            self.output.copy_from_slice(&self.result);
        } else {
            for r in 0..f {
                for c in 0..64 {
                    self.output[c * f + r] = self.result[r * 64 + c];
                }
            }
        }
    }
}
