//! Explicit Runge–Kutta integrators over `&mut [f64]`.
//!
//! - [`Method::Rk4`]: classical, fixed step.
//! - [`Method::Rkf78`]: Fehlberg 7(8), NASA TR R-287; propagates the 8th-order solution.
//! - [`Method::Dop853`]: Dormand–Prince 8(5,3), Hairer, Nørsett & Wanner, *Solving ODEs I*, §II.10;
//!   FSAL, 7th-order dense output (`contd8`).
//!
//! Adaptive error norm: RMS of `err_i / (atol + rtol·max(|y_i|, |y'_i|))`.

/// `f(t, y, dy/dt)`.
pub type Derivative<'a> = dyn FnMut(f64, &[f64], &mut [f64]) + 'a;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Method {
    /// Fixed step, s.
    Rk4 {
        step_s: f64,
    },
    Rkf78,
    Dop853,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tolerance {
    pub relative: f64,
    pub absolute: f64,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            relative: 1e-12,
            absolute: 1e-9,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IntegrationError {
    StepSizeUnderflow { t: f64, h: f64 },
    MaxSteps { t: f64 },
    NonFinite { t: f64 },
}

impl std::fmt::Display for IntegrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StepSizeUnderflow { t, h } => write!(f, "step size underflow at t={t} (h={h})"),
            Self::MaxSteps { t } => write!(f, "step limit reached at t={t}"),
            Self::NonFinite { t } => write!(f, "non-finite state at t={t}"),
        }
    }
}

impl std::error::Error for IntegrationError {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub accepted: usize,
    pub rejected: usize,
    pub evaluations: usize,
}

#[derive(Debug, Clone)]
pub struct Integrator {
    pub method: Method,
    pub tolerance: Tolerance,
    pub min_step_s: f64,
    pub max_step_s: f64,
    pub max_steps: usize,
    pub stats: Stats,
    /// Carried between calls.
    h_next: Option<f64>,
    k: Vec<Vec<f64>>,
    y_stage: Vec<f64>,
    y_new: Vec<f64>,
    err: Vec<f64>,
    /// Dense-output coefficients, one row per power.
    cont: Vec<Vec<f64>>,
}

impl Integrator {
    pub fn new(method: Method) -> Self {
        Self {
            method,
            tolerance: Tolerance::default(),
            min_step_s: 1e-6,
            max_step_s: f64::INFINITY,
            max_steps: 5_000_000,
            stats: Stats::default(),
            h_next: None,
            k: Vec::new(),
            y_stage: Vec::new(),
            y_new: Vec::new(),
            err: Vec::new(),
            cont: Vec::new(),
        }
    }

    pub fn with_tolerance(mut self, relative: f64, absolute: f64) -> Self {
        self.tolerance = Tolerance { relative, absolute };
        self
    }

    pub fn with_max_step(mut self, max_step_s: f64) -> Self {
        self.max_step_s = max_step_s;
        self
    }

    /// Drops the carried step size (call after a state discontinuity).
    pub fn reset(&mut self) {
        self.h_next = None;
    }

    /// Integrates `y` in place from `t0` to `t1` (either direction).
    pub fn integrate(
        &mut self,
        f: &mut Derivative,
        t0: f64,
        y: &mut [f64],
        t1: f64,
    ) -> Result<(), IntegrationError> {
        self.integrate_dense(f, t0, y, t1, &[], &mut |_, _| {})
    }

    /// As [`Self::integrate`], calling `on_output(t, y)` at each of `outputs` (strictly between
    /// `t0` and `t1`, ordered from `t0`). DOP853 interpolates; other methods stop on each.
    pub fn integrate_dense(
        &mut self,
        f: &mut Derivative,
        t0: f64,
        y: &mut [f64],
        t1: f64,
        outputs: &[f64],
        on_output: &mut dyn FnMut(f64, &[f64]),
    ) -> Result<(), IntegrationError> {
        if self.tableau().dense.is_some() {
            return self.run(f, t0, y, t1, outputs, on_output);
        }
        let mut t = t0;
        for &to in outputs {
            self.run(f, t, y, to, &[], on_output)?;
            on_output(to, y);
            t = to;
        }
        self.run(f, t, y, t1, &[], on_output)
    }

    fn run(
        &mut self,
        f: &mut Derivative,
        t0: f64,
        y: &mut [f64],
        t1: f64,
        outputs: &[f64],
        on_output: &mut dyn FnMut(f64, &[f64]),
    ) -> Result<(), IntegrationError> {
        let span = t1 - t0;
        if span == 0.0 {
            return Ok(());
        }
        let dir = span.signum();
        let n = y.len();
        let tab = self.tableau();
        // Dense methods: k[stages] = f(t + h, y_new), then the extra stages.
        let slots = tab.stages + tab.dense.map_or(0, |d| 1 + d.c.len());
        if self.k.len() < slots || self.k.first().is_some_and(|k| k.len() != n) {
            self.k = vec![vec![0.0; n]; slots];
        }
        self.y_stage.resize(n, 0.0);
        self.y_new.resize(n, 0.0);
        self.err.resize(n, 0.0);

        let mut t = t0;
        let mut h = match self.method {
            Method::Rk4 { step_s } => step_s.abs(),
            _ => match self.h_next {
                Some(h) => h.abs(),
                None => self.initial_step(f, t0, y, dir, tab.order),
            },
        };
        // k[0] = f(t, y) is current: reused after a rejection, and after acceptance for FSAL.
        let mut k0 = false;
        let mut next = 0;
        let mut steps = 0;
        while (t1 - t) * dir > 0.0 {
            steps += 1;
            if steps > self.max_steps {
                return Err(IntegrationError::MaxSteps { t });
            }
            h = h.min(self.max_step_s);
            let remaining = (t1 - t).abs();
            let last = h >= remaining;
            let h_try = if last { remaining } else { h };

            if !k0 {
                f(t, y, &mut self.k[0]);
                self.stats.evaluations += 1;
                k0 = true;
            }
            self.stage(f, &tab, t, y, dir * h_try);
            let finite = self.y_new.iter().all(|v| v.is_finite());
            let norm = match tab.e {
                Some(e) if finite => self.error_norm(y, e, h_try, tab.err3),
                Some(_) => f64::INFINITY,
                None if finite => 0.0,
                None => return Err(IntegrationError::NonFinite { t }),
            };
            // Non-finite trial: reject at the minimum factor (0.2).
            if norm.is_nan() || norm > 1.0 {
                self.stats.rejected += 1;
                h = h_try * (0.9 * norm.powf(-1.0 / (tab.order as f64 + 1.0))).max(0.2);
                if h < self.min_step_s {
                    return Err(if norm.is_finite() {
                        IntegrationError::StepSizeUnderflow { t, h }
                    } else {
                        IntegrationError::NonFinite { t }
                    });
                }
                continue;
            }

            let t_new = if last { t1 } else { t + dir * h_try };
            k0 = false;
            if let Some(d) = tab.dense {
                let pending = outputs
                    .get(next)
                    .is_some_and(|&to| (to - t_new) * dir <= 0.0);
                if !last || pending {
                    f(t_new, &self.y_new, &mut self.k[tab.stages]);
                    self.stats.evaluations += 1;
                    // Non-finite FSAL derivative: reject as a non-finite trial.
                    if !self.k[tab.stages].iter().all(|v| v.is_finite()) {
                        self.stats.rejected += 1;
                        h = h_try * 0.2;
                        if h < self.min_step_s {
                            return Err(IntegrationError::NonFinite { t });
                        }
                        k0 = true;
                        continue;
                    }
                    if pending {
                        self.dense(f, &tab, d, t, y, dir * h_try);
                    }
                    while let Some(&to) = outputs.get(next).filter(|&&to| (to - t_new) * dir <= 0.0)
                    {
                        self.interpolate(y, (to - t) / (dir * h_try));
                        on_output(to, &self.y_stage);
                        next += 1;
                    }
                    self.k.swap(0, tab.stages);
                    k0 = true;
                }
            }
            y.copy_from_slice(&self.y_new);
            t = t_new;
            self.stats.accepted += 1;
            if tab.e.is_some() {
                let grow = if norm == 0.0 {
                    10.0
                } else {
                    (0.9 * norm.powf(-1.0 / (tab.order as f64 + 1.0))).clamp(0.2, 10.0)
                };
                // Keep the pre-truncation step for the next call.
                h = if last {
                    h.max(h_try * grow)
                } else {
                    h_try * grow
                };
                self.h_next = Some(h);
            }
        }
        Ok(())
    }

    fn tableau(&self) -> Tableau {
        match self.method {
            Method::Rk4 { .. } => RK4,
            Method::Rkf78 => RKF78,
            Method::Dop853 => DOP853,
        }
    }

    /// `k[s] = f(t + c·h, y + h·Σ a_j k_j)`.
    fn eval_stage(&mut self, f: &mut Derivative, t: f64, y: &[f64], h: f64, s: usize, row: Row) {
        let (c, a) = row;
        self.y_stage.copy_from_slice(y);
        for (j, &a) in a.iter().enumerate() {
            if a != 0.0 {
                for (ys, kj) in self.y_stage.iter_mut().zip(&self.k[j]) {
                    *ys += h * a * kj;
                }
            }
        }
        f(t + c * h, &self.y_stage, &mut self.k[s]);
        self.stats.evaluations += 1;
    }

    /// Fills `k[1..]` and `y_new` for one step of size `h` (signed); `k[0]` is current.
    fn stage(&mut self, f: &mut Derivative, tab: &Tableau, t: f64, y: &[f64], h: f64) {
        for s in 1..tab.stages {
            self.eval_stage(f, t, y, h, s, (tab.c[s], tab.a[s]));
        }
        self.y_new.copy_from_slice(y);
        for (s, &b) in tab.b.iter().enumerate() {
            if b != 0.0 {
                for (yn, ks) in self.y_new.iter_mut().zip(&self.k[s]) {
                    *yn += h * b * ks;
                }
            }
        }
    }

    /// Extra stages and `cont` for the step `(t, y) → (t + h, y_new)`; `k[stages]` is current.
    fn dense(&mut self, f: &mut Derivative, tab: &Tableau, d: &Dense, t: f64, y: &[f64], h: f64) {
        for (e, (&c, a)) in d.c.iter().zip(d.a).enumerate() {
            self.eval_stage(f, t, y, h, tab.stages + 1 + e, (c, a));
        }
        let n = y.len();
        if self.cont.len() != 3 + d.d.len() || self.cont[0].len() != n {
            self.cont = vec![vec![0.0; n]; 3 + d.d.len()];
        }
        for (i, &yi) in y.iter().enumerate() {
            let ydiff = self.y_new[i] - yi;
            let bspl = h * self.k[0][i] - ydiff;
            self.cont[0][i] = ydiff;
            self.cont[1][i] = bspl;
            self.cont[2][i] = ydiff - h * self.k[tab.stages][i] - bspl;
            for (r, row) in d.d.iter().enumerate() {
                let sum: f64 = row
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| **c != 0.0)
                    .map(|(j, c)| c * self.k[j][i])
                    .sum();
                self.cont[3 + r][i] = h * sum;
            }
        }
    }

    /// `y_stage` = dense output at step fraction `s` from `y`.
    fn interpolate(&mut self, y: &[f64], s: f64) {
        let s1 = 1.0 - s;
        let c = &self.cont;
        for (i, out) in self.y_stage.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (r, row) in c.iter().enumerate().rev() {
                acc = (acc + row[i]) * if r % 2 == 0 { s } else { s1 };
            }
            *out = y[i] + acc;
        }
    }

    fn error_norm(&mut self, y: &[f64], e: &[f64], h: f64, err3: Option<&[f64]>) -> f64 {
        let tol = self.tolerance;
        let n = y.len() as f64;
        let scale = |i: usize| tol.absolute + tol.relative * y[i].abs().max(self.y_new[i].abs());
        let weighted = |coef: &[f64], i: usize| -> f64 {
            let sum = coef
                .iter()
                .enumerate()
                .filter(|(_, c)| **c != 0.0)
                .map(|(s, c)| c * self.k[s][i])
                .sum::<f64>();
            scaled(sum, scale(i))
        };
        let e5: f64 = (0..y.len()).map(|i| weighted(e, i).powi(2)).sum();
        match err3 {
            None => h * (e5 / n).sqrt(),
            Some(e3c) => {
                // Hairer's combined 5th/3rd-order estimate.
                let e3: f64 = (0..y.len()).map(|i| weighted(e3c, i).powi(2)).sum();
                if e5 == 0.0 && e3 == 0.0 {
                    return 0.0;
                }
                h * e5 / ((e5 + 0.01 * e3) * n).sqrt()
            }
        }
    }

    /// Hairer, *Solving ODEs I*, §II.4.
    fn initial_step(
        &mut self,
        f: &mut Derivative,
        t0: f64,
        y: &[f64],
        dir: f64,
        order: usize,
    ) -> f64 {
        let n = y.len();
        let tol = self.tolerance;
        let sc: Vec<f64> = y
            .iter()
            .map(|v| tol.absolute + tol.relative * v.abs())
            .collect();
        let rms = |v: &[f64]| {
            (v.iter()
                .zip(&sc)
                .map(|(a, s)| scaled(*a, *s).powi(2))
                .sum::<f64>()
                / n as f64)
                .sqrt()
        };
        let mut f0 = vec![0.0; n];
        f(t0, y, &mut f0);
        let (d0, d1) = (rms(y), rms(&f0));
        let h0 = if d0 < 1e-5 || d1 < 1e-5 {
            1e-6
        } else {
            0.01 * d0 / d1
        };
        let y1: Vec<f64> = y.iter().zip(&f0).map(|(a, b)| a + dir * h0 * b).collect();
        let mut f1 = vec![0.0; n];
        f(t0 + dir * h0, &y1, &mut f1);
        self.stats.evaluations += 2;
        let diff: Vec<f64> = f1.iter().zip(&f0).map(|(a, b)| a - b).collect();
        let d2 = rms(&diff) / h0;
        let h1 = if d1.max(d2) <= 1e-15 {
            (h0 * 1e-3).max(1e-6)
        } else {
            (0.01 / d1.max(d2)).powf(1.0 / (order as f64 + 1.0))
        };
        (100.0 * h0).min(h1).max(self.min_step_s)
    }
}

/// `a / s`; 0 for zero scale (`absolute = 0` with a zero component).
fn scaled(a: f64, s: f64) -> f64 {
    if s == 0.0 { 0.0 } else { a / s }
}

/// Stage node and coefficients `(c, a)`.
type Row = (f64, &'static [f64]);

struct Tableau {
    stages: usize,
    /// Order of the propagated solution.
    order: usize,
    c: &'static [f64],
    a: &'static [&'static [f64]],
    b: &'static [f64],
    /// Error weights `b − b̂`; `None` for fixed step.
    e: Option<&'static [f64]>,
    /// DOP853 3rd-order error weights.
    err3: Option<&'static [f64]>,
    dense: Option<&'static Dense>,
}

/// Dense output: extra stages (`c`, `a` over all prior `k`) and the `d` rows over all `k`.
struct Dense {
    c: [f64; 3],
    a: [&'static [f64]; 3],
    d: [&'static [f64]; 4],
}

const RK4: Tableau = Tableau {
    stages: 4,
    order: 4,
    c: &[0.0, 0.5, 0.5, 1.0],
    a: &[&[], &[0.5], &[0.0, 0.5], &[0.0, 0.0, 1.0]],
    b: &[1.0 / 6.0, 1.0 / 3.0, 1.0 / 3.0, 1.0 / 6.0],
    e: None,
    err3: None,
    dense: None,
};

const RKF78: Tableau = Tableau {
    stages: 13,
    order: 7,
    c: &[
        0.0,
        2.0 / 27.0,
        1.0 / 9.0,
        1.0 / 6.0,
        5.0 / 12.0,
        0.5,
        5.0 / 6.0,
        1.0 / 6.0,
        2.0 / 3.0,
        1.0 / 3.0,
        1.0,
        0.0,
        1.0,
    ],
    a: &[
        &[],
        &[2.0 / 27.0],
        &[1.0 / 36.0, 1.0 / 12.0],
        &[1.0 / 24.0, 0.0, 1.0 / 8.0],
        &[5.0 / 12.0, 0.0, -25.0 / 16.0, 25.0 / 16.0],
        &[1.0 / 20.0, 0.0, 0.0, 1.0 / 4.0, 1.0 / 5.0],
        &[
            -25.0 / 108.0,
            0.0,
            0.0,
            125.0 / 108.0,
            -65.0 / 27.0,
            125.0 / 54.0,
        ],
        &[
            31.0 / 300.0,
            0.0,
            0.0,
            0.0,
            61.0 / 225.0,
            -2.0 / 9.0,
            13.0 / 900.0,
        ],
        &[
            2.0,
            0.0,
            0.0,
            -53.0 / 6.0,
            704.0 / 45.0,
            -107.0 / 9.0,
            67.0 / 90.0,
            3.0,
        ],
        &[
            -91.0 / 108.0,
            0.0,
            0.0,
            23.0 / 108.0,
            -976.0 / 135.0,
            311.0 / 54.0,
            -19.0 / 60.0,
            17.0 / 6.0,
            -1.0 / 12.0,
        ],
        &[
            2383.0 / 4100.0,
            0.0,
            0.0,
            -341.0 / 164.0,
            4496.0 / 1025.0,
            -301.0 / 82.0,
            2133.0 / 4100.0,
            45.0 / 82.0,
            45.0 / 164.0,
            18.0 / 41.0,
        ],
        &[
            3.0 / 205.0,
            0.0,
            0.0,
            0.0,
            0.0,
            -6.0 / 41.0,
            -3.0 / 205.0,
            -3.0 / 41.0,
            3.0 / 41.0,
            6.0 / 41.0,
            0.0,
        ],
        &[
            -1777.0 / 4100.0,
            0.0,
            0.0,
            -341.0 / 164.0,
            4496.0 / 1025.0,
            -289.0 / 82.0,
            2193.0 / 4100.0,
            51.0 / 82.0,
            33.0 / 164.0,
            12.0 / 41.0,
            0.0,
            1.0,
        ],
    ],
    // 8th order.
    b: &[
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        34.0 / 105.0,
        9.0 / 35.0,
        9.0 / 35.0,
        9.0 / 280.0,
        9.0 / 280.0,
        0.0,
        41.0 / 840.0,
        41.0 / 840.0,
    ],
    // b8 − b7 = 41/840 · (−1, 0…, −1 at 10, +1 at 11, +1 at 12).
    e: Some(&[
        -41.0 / 840.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        -41.0 / 840.0,
        41.0 / 840.0,
        41.0 / 840.0,
    ]),
    err3: None,
    dense: None,
};

#[allow(clippy::excessive_precision)] // Hairer's coefficients verbatim
const DOP853: Tableau = Tableau {
    stages: 12,
    order: 7,
    c: &[
        0.0,
        0.526001519587677318785587544488e-1,
        0.789002279381515978178381316732e-1,
        0.118350341907227396726757197510,
        0.281649658092772603273242802490,
        0.333333333333333333333333333333,
        0.25,
        0.307692307692307692307692307692,
        0.651282051282051282051282051282,
        0.6,
        0.857142857142857142857142857142,
        1.0,
    ],
    a: &[
        &[],
        &[5.26001519587677318785587544488e-2],
        &[
            1.97250569845378994544595329183e-2,
            5.91751709536136983633785987549e-2,
        ],
        &[
            2.95875854768068491816892993775e-2,
            0.0,
            8.87627564304205475450678981324e-2,
        ],
        &[
            2.41365134159266685502369798665e-1,
            0.0,
            -8.84549479328286085344864962717e-1,
            9.24834003261792003115737966543e-1,
        ],
        &[
            3.7037037037037037037037037037e-2,
            0.0,
            0.0,
            1.70828608729473871279604482173e-1,
            1.25467687566822425016691814123e-1,
        ],
        &[
            3.7109375e-2,
            0.0,
            0.0,
            1.70252211019544039314978060272e-1,
            6.02165389804559606850219397283e-2,
            -1.7578125e-2,
        ],
        &[
            3.70920001185047927108779319836e-2,
            0.0,
            0.0,
            1.70383925712239993810214054705e-1,
            1.07262030446373284651809199168e-1,
            -1.53194377486244017527936158236e-2,
            8.27378916381402288758473766002e-3,
        ],
        &[
            6.24110958716075717114429577812e-1,
            0.0,
            0.0,
            -3.36089262944694129406857109825,
            -8.68219346841726006818189891453e-1,
            2.75920996994467083049415600797e1,
            2.01540675504778934086186788979e1,
            -4.34898841810699588477366255144e1,
        ],
        &[
            4.77662536438264365890433908527e-1,
            0.0,
            0.0,
            -2.48811461997166764192642586468,
            -5.90290826836842996371446475743e-1,
            2.12300514481811942347288949897e1,
            1.52792336328824235832596922938e1,
            -3.32882109689848629194453265587e1,
            -2.03312017085086261358222928593e-2,
        ],
        &[
            -9.3714243008598732571704021658e-1,
            0.0,
            0.0,
            5.18637242884406370830023853209,
            1.09143734899672957818500254654,
            -8.14978701074692612513997267357,
            -1.85200656599969598641566180701e1,
            2.27394870993505042818970056734e1,
            2.49360555267965238987089396762,
            -3.0467644718982195003823669022,
        ],
        &[
            2.27331014751653820792359768449,
            0.0,
            0.0,
            -1.05344954667372501984066689879e1,
            -2.00087205822486249909675718444,
            -1.79589318631187989172765950534e1,
            2.79488845294199600508499808837e1,
            -2.85899827713502369474065508674,
            -8.87285693353062954433549289258,
            1.23605671757943030647266201528e1,
            6.43392746015763530355970484046e-1,
        ],
    ],
    b: &DOP853_B,
    e: Some(&[
        0.1312004499419488073250102996e-1,
        0.0,
        0.0,
        0.0,
        0.0,
        -0.1225156446376204440720569753e+1,
        -0.4957589496572501915214079952,
        0.1664377182454986536961530415e+1,
        -0.3503288487499736816886487290,
        0.3341791187130174790297318841,
        0.8192320648511571246570742613e-1,
        -0.2235530786388629525884427845e-1,
    ]),
    err3: Some(&[
        DOP853_B[0] - 0.244094488188976377952755905512,
        0.0,
        0.0,
        0.0,
        0.0,
        DOP853_B[5],
        DOP853_B[6],
        DOP853_B[7],
        DOP853_B[8] - 0.733846688281611857341361741547,
        DOP853_B[9],
        DOP853_B[10],
        DOP853_B[11] - 0.220588235294117647058823529412e-1,
    ]),
    dense: Some(&DOP853_DENSE),
};

#[allow(clippy::excessive_precision)]
const DOP853_B: [f64; 12] = [
    5.42937341165687622380535766363e-2,
    0.0,
    0.0,
    0.0,
    0.0,
    4.45031289275240888144113950566,
    1.89151789931450038304281599044,
    -5.8012039600105847814672114227,
    3.1116436695781989440891606237e-1,
    -1.52160949662516078556178806805e-1,
    2.01365400804030348374776537501e-1,
    4.47106157277725905176885569043e-2,
];

#[allow(clippy::excessive_precision)] // Hairer's dop853.f (contd8), as tabulated in SciPy
const DOP853_DENSE: Dense = Dense {
    c: [0.1, 0.2, 0.777777777777777777777777777778],
    a: [
        &[
            5.61675022830479523392909219681e-2,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            2.53500210216624811088794765333e-1,
            -2.46239037470802489917441475441e-1,
            -1.24191423263816360469010140626e-1,
            1.5329179827876569731206322685e-1,
            8.20105229563468988491666602057e-3,
            7.56789766054569976138603589584e-3,
            -8.298e-3,
        ],
        &[
            3.18346481635021405060768473261e-2,
            0.0,
            0.0,
            0.0,
            0.0,
            2.83009096723667755288322961402e-2,
            5.35419883074385676223797384372e-2,
            -5.49237485713909884646569340306e-2,
            0.0,
            0.0,
            -1.08347328697249322858509316994e-4,
            3.82571090835658412954920192323e-4,
            -3.40465008687404560802977114492e-4,
            1.41312443674632500278074618366e-1,
        ],
        &[
            -4.28896301583791923408573538692e-1,
            0.0,
            0.0,
            0.0,
            0.0,
            -4.69762141536116384314449447206,
            7.68342119606259904184240953878,
            4.06898981839711007970213554331,
            3.56727187455281109270669543021e-1,
            0.0,
            0.0,
            0.0,
            -1.39902416515901462129418009734e-3,
            2.9475147891527723389556272149,
            -9.15095847217987001081870187138,
        ],
    ],
    d: [
        &[
            -0.84289382761090128651353491142e+1,
            0.0,
            0.0,
            0.0,
            0.0,
            0.56671495351937776962531783590,
            -0.30689499459498916912797304727e+1,
            0.23846676565120698287728149680e+1,
            0.21170345824450282767155149946e+1,
            -0.87139158377797299206789907490,
            0.22404374302607882758541771650e+1,
            0.63157877876946881815570249290,
            -0.88990336451333310820698117400e-1,
            0.18148505520854727256656404962e+2,
            -0.91946323924783554000451984436e+1,
            -0.44360363875948939664310572000e+1,
        ],
        &[
            0.10427508642579134603413151009e+2,
            0.0,
            0.0,
            0.0,
            0.0,
            0.24228349177525818288430175319e+3,
            0.16520045171727028198505394887e+3,
            -0.37454675472269020279518312152e+3,
            -0.22113666853125306036270938578e+2,
            0.77334326684722638389603898808e+1,
            -0.30674084731089398182061213626e+2,
            -0.93321305264302278729567221706e+1,
            0.15697238121770843886131091075e+2,
            -0.31139403219565177677282850411e+2,
            -0.93529243588444783865713862664e+1,
            0.35816841486394083752465898540e+2,
        ],
        &[
            0.19985053242002433820987653617e+2,
            0.0,
            0.0,
            0.0,
            0.0,
            -0.38703730874935176555105901742e+3,
            -0.18917813819516756882830838328e+3,
            0.52780815920542364900561016686e+3,
            -0.11573902539959630126141871134e+2,
            0.68812326946963000169666922661e+1,
            -0.10006050966910838403183860980e+1,
            0.77771377980534432092869265740,
            -0.27782057523535084065932004339e+1,
            -0.60196695231264120758267380846e+2,
            0.84320405506677161018159903784e+2,
            0.11992291136182789328035130030e+2,
        ],
        &[
            -0.25693933462703749003312586129e+2,
            0.0,
            0.0,
            0.0,
            0.0,
            -0.15418974869023643374053993627e+3,
            -0.23152937917604549567536039109e+3,
            0.35763911791061412378285349910e+3,
            0.93405324183624310003907691704e+2,
            -0.37458323136451633156875139351e+2,
            0.10409964950896230045147246184e+3,
            0.29840293426660503123344363579e+2,
            -0.43533456590011143754432175058e+2,
            0.96324553959188282948394950600e+2,
            -0.39177261675615439165231486172e+2,
            -0.14972683625798562581422125276e+3,
        ],
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn tableaus() -> [(&'static str, Tableau); 3] {
        [("rk4", RK4), ("rkf78", RKF78), ("dop853", DOP853)]
    }

    #[test]
    fn tableaus_are_consistent() {
        for (name, t) in tableaus() {
            assert_eq!(t.c.len(), t.stages, "{name}");
            assert_eq!(t.a.len(), t.stages, "{name}");
            assert_eq!(t.b.len(), t.stages, "{name}");
            for (i, row) in t.a.iter().enumerate() {
                let sum: f64 = row.iter().sum();
                assert!(
                    (sum - t.c[i]).abs() < 1e-14,
                    "{name} row {i}: {sum} vs {}",
                    t.c[i]
                );
            }
            // Quadrature conditions Σ b c^(q−1) = 1/q up to the method order.
            for q in 1..=t.order.min(8) {
                let s: f64 =
                    t.b.iter()
                        .zip(t.c)
                        .map(|(b, c)| b * c.powi(q as i32 - 1))
                        .sum();
                assert!((s - 1.0 / q as f64).abs() < 1e-13, "{name} q={q}: {s}");
            }
            if let Some(e) = t.e {
                assert!(e.iter().sum::<f64>().abs() < 1e-14, "{name} Σe");
            }
            if let Some(d) = t.dense {
                for (a, c) in d.a.iter().zip(d.c) {
                    assert!(
                        (a.iter().sum::<f64>() - c).abs() < 1e-14,
                        "{name} dense c={c}"
                    );
                }
            }
        }
    }

    /// Harmonic oscillator over 10 periods.
    fn oscillator(method: Method) -> (f64, Stats) {
        let mut integ = Integrator::new(method).with_tolerance(1e-12, 1e-12);
        let mut f = |_t: f64, y: &[f64], d: &mut [f64]| {
            d[0] = y[1];
            d[1] = -y[0];
        };
        let mut y = [1.0, 0.0];
        let t1 = 20.0 * std::f64::consts::PI;
        integ.integrate(&mut f, 0.0, &mut y, t1).unwrap();
        (
            ((y[0] - t1.cos()).powi(2) + (y[1] + t1.sin()).powi(2)).sqrt(),
            integ.stats,
        )
    }

    #[test]
    fn adaptive_methods_meet_tolerance() {
        for m in [Method::Rkf78, Method::Dop853] {
            let (err, stats) = oscillator(m);
            assert!(err < 1e-9, "{m:?}: {err}");
            assert!(stats.accepted < 2000, "{m:?}: {stats:?}");
        }
    }

    #[test]
    fn dense_output_matches_solution() {
        for m in [Method::Dop853, Method::Rkf78] {
            let mut integ = Integrator::new(m).with_tolerance(1e-12, 1e-12);
            let mut f = |_t: f64, y: &[f64], d: &mut [f64]| {
                d[0] = y[1];
                d[1] = -y[0];
            };
            let mut y = [1.0, 0.0];
            let outputs: Vec<f64> = (1..200).map(|i| i as f64 * 0.1).collect();
            let mut seen = Vec::new();
            integ
                .integrate_dense(&mut f, 0.0, &mut y, 20.0, &outputs, &mut |t, y| {
                    let err = ((y[0] - t.cos()).powi(2) + (y[1] + t.sin()).powi(2)).sqrt();
                    assert!(err < 1e-10, "{m:?} t={t}: {err}");
                    seen.push(t);
                })
                .unwrap();
            assert_eq!(seen, outputs, "{m:?}");
            if m == Method::Dop853 {
                // Steps are not shortened by outputs.
                let mut plain = Integrator::new(m).with_tolerance(1e-12, 1e-12);
                plain.integrate(&mut f, 0.0, &mut [1.0, 0.0], 20.0).unwrap();
                assert_eq!(integ.stats.accepted, plain.stats.accepted);
            }
        }
    }

    #[test]
    fn rk4_is_fourth_order() {
        let (e1, _) = oscillator(Method::Rk4 { step_s: 0.02 });
        let (e2, _) = oscillator(Method::Rk4 { step_s: 0.01 });
        let p = (e1 / e2).log2();
        assert!((p - 4.0).abs() < 0.2, "{p}");
    }

    #[test]
    fn non_finite_trial_is_rejected() {
        // NaN for y < 0, reached only by an oversized step.
        let mut f = |_t: f64, y: &[f64], d: &mut [f64]| {
            d[0] = if y[0] < 0.0 { f64::NAN } else { -y[0] };
        };
        for m in [Method::Rkf78, Method::Dop853] {
            let mut integ = Integrator::new(m);
            integ.h_next = Some(100.0);
            let mut y = [1.0];
            integ.integrate(&mut f, 0.0, &mut y, 5.0).unwrap();
            assert!(integ.stats.rejected > 0, "{m:?}");
            assert!((y[0] - (-5.0f64).exp()).abs() < 1e-9, "{m:?}: {}", y[0]);
        }
    }

    #[test]
    fn zero_absolute_tolerance_zero_component() {
        // Pure relative tolerance; y[1] stays exactly 0.
        let mut f = |_t: f64, y: &[f64], d: &mut [f64]| {
            d[0] = -y[0];
            d[1] = 0.0;
        };
        for m in [Method::Rkf78, Method::Dop853] {
            let mut y = [1.0, 0.0];
            Integrator::new(m)
                .with_tolerance(1e-10, 0.0)
                .integrate(&mut f, 0.0, &mut y, 2.0)
                .unwrap();
            assert!((y[0] - (-2.0f64).exp()).abs() < 1e-9, "{m:?}: {}", y[0]);
            assert_eq!(y[1], 0.0, "{m:?}");
        }
    }

    #[test]
    fn persistent_non_finite_errors() {
        let mut f = |t: f64, _y: &[f64], d: &mut [f64]| {
            d[0] = if t > 1.0 { f64::NAN } else { 1.0 };
        };
        for m in [Method::Rk4 { step_s: 0.3 }, Method::Rkf78, Method::Dop853] {
            let err = Integrator::new(m)
                .integrate(&mut f, 0.0, &mut [0.0], 2.0)
                .unwrap_err();
            assert!(
                matches!(err, IntegrationError::NonFinite { .. }),
                "{m:?}: {err}"
            );
        }
    }

    #[test]
    fn non_finite_fsal_errors() {
        // Finite step endpoint, NaN derivative there.
        let mut f = |_t: f64, y: &[f64], d: &mut [f64]| {
            d[0] = if y[0] < 0.5 { f64::NAN } else { -1.0 };
        };
        let mut out = Vec::new();
        let err = Integrator::new(Method::Dop853)
            .integrate_dense(&mut f, 0.0, &mut [1.0], 1.0, &[0.3, 0.6], &mut |t, y| {
                out.push((t, y[0]))
            })
            .unwrap_err();
        assert!(matches!(err, IntegrationError::NonFinite { .. }), "{err}");
        assert!(out.iter().all(|(_, y)| y.is_finite()), "{out:?}");
    }

    #[test]
    fn integrates_backward() {
        let mut integ = Integrator::new(Method::Dop853);
        let mut f = |_t: f64, y: &[f64], d: &mut [f64]| d[0] = y[0];
        let mut y = [1.0];
        integ.integrate(&mut f, 0.0, &mut y, -2.0).unwrap();
        assert!((y[0] - (-2.0f64).exp()).abs() < 1e-9);
    }
}
