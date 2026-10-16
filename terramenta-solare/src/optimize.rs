//! Constrained minimization: trust-region Sℓ1QP (Fletcher).
//!
//! minimize f(x) s.t. h(x) = 0, g(x) ≤ 0, lower ≤ x ≤ upper.
//!
//! - Scaling: x by perturbation, f by the objective tolerance, h and g by their tolerances.
//! - Jacobians: forward difference (backward at an upper bound), one evaluation per variable,
//!   reused across rejected steps.
//! - Hessian: BFGS on the Lagrangian, Powell-damped, Shanno–Phua scaled at the first update.
//! - Subproblem: quadratic model of the ℓ1 merit f + σ(Σ|h| + Σ max(0, g)) with linearized
//!   constraints, in a box of radius τ·`max_step`; always feasible. Solved as a QP with elastic
//!   variables by primal-dual interior point (Mehrotra).
//! - σ: raised when the step makes no progress on linearized feasibility, relaxed toward the
//!   multipliers once the linearization is feasible.
//! - Steps: accepted on actual / predicted merit reduction ≥ 0.1 (second-order correction
//!   tried on rejection); τ halved below that, doubled above 0.75 at the box edge.
//! - Converged: every |h| and g within tolerance, predicted |Δf| ≤ objective tolerance with
//!   the step inside the box.

/// One varied parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Variable {
    pub value: f64,
    /// Finite-difference step; also the variable's scale.
    pub perturbation: f64,
    /// Largest change per iteration.
    pub max_step: f64,
    pub lower: f64,
    pub upper: f64,
}

impl Variable {
    /// Unbounded.
    pub fn new(value: f64, perturbation: f64, max_step: f64) -> Self {
        Self {
            value,
            perturbation,
            max_step,
            lower: f64::NEG_INFINITY,
            upper: f64::INFINITY,
        }
    }

    pub fn bounded(mut self, lower: f64, upper: f64) -> Self {
        self.lower = lower;
        self.upper = upper;
        self
    }
}

/// f, h, g at one point.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Evaluation {
    pub objective: f64,
    pub equalities: Vec<f64>,
    /// Satisfied when ≤ 0.
    pub inequalities: Vec<f64>,
}

/// Convergence scales, unscaled units.
#[derive(Debug, Clone, PartialEq)]
pub struct Tolerances {
    pub objective: f64,
    pub equalities: Vec<f64>,
    pub inequalities: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub objective: f64,
    /// Largest |h / tol| or g / tol, ≥ 0; ≤ 1 is feasible.
    pub violation: f64,
}

/// Outcome of [`minimize`].
#[derive(Debug, Clone, PartialEq)]
pub struct Solution {
    pub values: Vec<f64>,
    /// At `values`, unscaled.
    pub evaluation: Evaluation,
    /// Feasible and stationary.
    pub converged: bool,
    /// Every constraint within tolerance.
    pub feasible: bool,
    pub iterations: usize,
    /// Calls of the evaluation function.
    pub evaluations: usize,
    /// Initial point, then after each step.
    pub history: Vec<Progress>,
}

/// Step acceptance and expansion ratios.
const ACCEPT: f64 = 0.1;
const EXPAND: f64 = 0.75;
/// Smallest box radius, fraction of `max_step`.
const MIN_RADIUS: f64 = 1e-9;

struct Point {
    x: Vec<f64>,
    raw: Evaluation,
    f: f64,
    h: Vec<f64>,
    g: Vec<f64>,
}

impl Point {
    /// Σ|h| + Σ max(0, g), scaled.
    fn l1(&self) -> f64 {
        self.h.iter().map(|v| v.abs()).sum::<f64>() + self.g.iter().map(|v| v.max(0.0)).sum::<f64>()
    }

    fn worst(&self) -> f64 {
        self.h
            .iter()
            .map(|v| v.abs())
            .chain(self.g.iter().copied())
            .fold(0.0, f64::max)
    }

    fn progress(&self) -> Progress {
        Progress {
            objective: self.raw.objective,
            violation: self.worst(),
        }
    }
}

/// Scaled Jacobians at a point: ∂/∂(x / p).
struct Jacobian {
    f: Vec<f64>,
    h: Vec<Vec<f64>>,
    g: Vec<Vec<f64>>,
}

impl Jacobian {
    /// ∇f + Hᵀλ + Gᵀμ.
    fn lagrangian(&self, lambda: &[f64], mu: &[f64]) -> Vec<f64> {
        let mut out = self.f.clone();
        for (row, l) in self.h.iter().zip(lambda).chain(self.g.iter().zip(mu)) {
            for (o, r) in out.iter_mut().zip(row) {
                *o += r * l;
            }
        }
        out
    }
}

/// Minimizes `evaluate(x).objective` subject to its constraints and the variable bounds.
/// An `Err` at the initial point or a Jacobian column is returned; at a trial step it rejects
/// the step.
pub fn minimize<E>(
    mut evaluate: impl FnMut(&[f64]) -> Result<Evaluation, E>,
    variables: &[Variable],
    tolerances: &Tolerances,
    max_iterations: usize,
) -> Result<Solution, E> {
    let n = variables.len();
    let (me, mi) = (tolerances.equalities.len(), tolerances.inequalities.len());
    let mut evaluations = 0;
    let mut point = |x: Vec<f64>, evaluations: &mut usize| -> Result<Point, E> {
        *evaluations += 1;
        let raw = evaluate(&x)?;
        let scale = |v: &[f64], t: &[f64]| v.iter().zip(t).map(|(v, t)| v / t).collect();
        Ok(Point {
            f: raw.objective / tolerances.objective,
            h: scale(&raw.equalities, &tolerances.equalities),
            g: scale(&raw.inequalities, &tolerances.inequalities),
            raw,
            x,
        })
    };
    let finite = |p: &Point| p.f.is_finite() && p.h.iter().chain(&p.g).all(|v| v.is_finite());

    let x0 = variables
        .iter()
        .map(|v| v.value.clamp(v.lower, v.upper))
        .collect();
    let mut current = point(x0, &mut evaluations)?;
    let mut history = vec![current.progress()];
    let mut iterations = 0;
    let mut converged = false;

    let mut hessian: Option<Vec<Vec<f64>>> = None;
    let mut previous: Option<(Vec<f64>, Jacobian)> = None;
    let (mut lambda, mut mu) = (vec![0.0; me], vec![0.0; mi]);
    let mut sigma = 1.0_f64;
    let mut radius = 1.0_f64;
    let mut cached: Option<Jacobian> = None;
    // Rejections between accepted steps are bounded by the radius floor; this bounds the rest.
    let mut solves = 0;

    while n > 0 && solves < 20 * (max_iterations + 1) {
        solves += 1;
        let jac = match &mut cached {
            Some(j) => j,
            None => {
                let mut fresh = Jacobian {
                    f: vec![0.0; n],
                    h: vec![vec![0.0; n]; me],
                    g: vec![vec![0.0; n]; mi],
                };
                for (j, v) in variables.iter().enumerate() {
                    let sign = if current.x[j] + v.perturbation > v.upper {
                        -1.0
                    } else {
                        1.0
                    };
                    let mut x = current.x.clone();
                    x[j] += sign * v.perturbation;
                    let probe = point(x, &mut evaluations)?;
                    fresh.f[j] = sign * (probe.f - current.f);
                    for (row, (a, b)) in fresh.h.iter_mut().zip(probe.h.iter().zip(&current.h)) {
                        row[j] = sign * (a - b);
                    }
                    for (row, (a, b)) in fresh.g.iter_mut().zip(probe.g.iter().zip(&current.g)) {
                        row[j] = sign * (a - b);
                    }
                }
                let z: Vec<f64> = scaled(&current.x, variables);
                match (&mut hessian, &previous) {
                    (Some(b), Some((z_prev, jac_prev))) => {
                        let s: Vec<f64> = z.iter().zip(z_prev).map(|(a, b)| a - b).collect();
                        let y: Vec<f64> = fresh
                            .lagrangian(&lambda, &mu)
                            .iter()
                            .zip(jac_prev.lagrangian(&lambda, &mu))
                            .map(|(a, b)| a - b)
                            .collect();
                        bfgs(b, &s, &y, iterations == 1);
                    }
                    (None, _) => {
                        let gnorm = fresh.f.iter().map(|v| v * v).sum::<f64>().sqrt();
                        let dnorm = variables
                            .iter()
                            .map(|v| (v.max_step / v.perturbation).powi(2))
                            .sum::<f64>()
                            .sqrt();
                        hessian = Some(identity(n, (gnorm / dnorm).max(1e-6)));
                    }
                    _ => {}
                }
                cached.insert(fresh)
            }
        };
        let box_: Vec<(f64, f64)> = variables
            .iter()
            .zip(&current.x)
            .map(|(v, x)| {
                let d = radius * v.max_step / v.perturbation;
                (
                    (-d).max((v.lower - x) / v.perturbation),
                    d.min((v.upper - x) / v.perturbation),
                )
            })
            .collect();
        let b = hessian.as_ref().expect("set");

        // Subproblem; σ raised while it makes no progress on linearized feasibility.
        let violation = current.l1();
        let mut qp = None;
        for _ in 0..4 {
            let Some(step) = subproblem(b, jac, &current, &box_, sigma) else {
                break;
            };
            let binding = step.multiplier >= 0.99 * sigma;
            let stalled = violation - step.linear_violation < 0.1 * violation;
            let retry = binding && stalled && violation > 0.0 && sigma < 1e12;
            qp = Some(step);
            if !retry {
                break;
            }
            sigma *= 10.0;
        }
        let Some(qp) = qp else {
            break;
        };
        let predicted: f64 = jac.f.iter().zip(&qp.d).map(|(g, d)| g * d).sum();
        let interior =
            qp.d.iter()
                .zip(&box_)
                .all(|(d, (lo, hi))| *d > 0.99 * lo && *d < 0.99 * hi);
        if current.worst() <= 1.0 && predicted.abs() <= 1.0 && (interior || radius >= 1.0) {
            converged = true;
            break;
        }
        if iterations >= max_iterations {
            break;
        }
        let quadratic = 0.5
            * qp.d
                .iter()
                .zip(b)
                .map(|(d, row)| d * row.iter().zip(&qp.d).map(|(r, e)| r * e).sum::<f64>())
                .sum::<f64>();
        let model = -(predicted + quadratic) + sigma * (violation - qp.linear_violation);
        if model.is_nan() || model <= 0.0 {
            break;
        }
        let merit0 = current.f + sigma * violation;
        let to_x = |step: &[f64]| -> Vec<f64> {
            current
                .x
                .iter()
                .zip(step)
                .zip(variables)
                .map(|((x, d), v)| (x + d * v.perturbation).clamp(v.lower, v.upper))
                .collect()
        };
        let ratio = |trial: &Point| {
            if finite(trial) {
                (merit0 - trial.f - sigma * trial.l1()) / model
            } else {
                f64::NEG_INFINITY
            }
        };

        let mut accepted = None;
        if let Ok(trial) = point(to_x(&qp.d), &mut evaluations) {
            let r = ratio(&trial);
            if r >= ACCEPT {
                accepted = Some((trial, r));
            } else if finite(&trial)
                && let Some(dc) = correction(jac, &trial)
            {
                // Second-order correction against constraint curvature (Maratos).
                let step: Vec<f64> = qp.d.iter().zip(&dc).map(|(d, c)| d + c).collect();
                if let Ok(soc) = point(to_x(&step), &mut evaluations) {
                    let r = ratio(&soc);
                    if r >= ACCEPT {
                        accepted = Some((soc, r));
                    }
                }
            }
        }
        let extent =
            qp.d.iter()
                .zip(variables)
                .map(|(d, v)| d.abs() * v.perturbation / v.max_step)
                .fold(0.0, f64::max);
        let Some((trial, r)) = accepted else {
            radius = 0.5 * extent.min(radius);
            if radius < MIN_RADIUS {
                break;
            }
            continue;
        };
        if r > EXPAND && extent > 0.99 * radius {
            radius = (2.0 * radius).min(1.0);
        }
        // Relax σ toward the multipliers once the linearization is feasible.
        if qp.linear_violation <= 1e-9 * (1.0 + violation) {
            sigma = sigma.min((2.0 * qp.multiplier).max(1.0));
        }
        (lambda, mu) = (qp.lambda, qp.mu);
        previous = Some((scaled(&current.x, variables), cached.take().expect("set")));
        current = trial;
        iterations += 1;
        history.push(current.progress());
    }

    let feasible = current.worst() <= 1.0;
    Ok(Solution {
        values: current.x,
        evaluation: current.raw,
        converged: converged && feasible,
        feasible,
        iterations,
        evaluations,
        history,
    })
}

fn scaled(x: &[f64], variables: &[Variable]) -> Vec<f64> {
    x.iter()
        .zip(variables)
        .map(|(x, v)| x / v.perturbation)
        .collect()
}

/// Minimum-norm −Jᵀ(JJᵀ)⁻¹c over equalities and violated inequalities at `trial`, with the
/// current Jacobian. `None` when nothing is violated or JJᵀ is singular.
fn correction(jac: &Jacobian, trial: &Point) -> Option<Vec<f64>> {
    let rows: Vec<(&Vec<f64>, f64)> = jac
        .h
        .iter()
        .zip(trial.h.iter().copied())
        .chain(
            jac.g
                .iter()
                .zip(trial.g.iter().copied())
                .filter(|(_, g)| *g > 0.0),
        )
        .collect();
    if rows.is_empty() {
        return None;
    }
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(a, b)| a * b).sum::<f64>();
    let jjt = rows
        .iter()
        .map(|(a, _)| rows.iter().map(|(b, _)| dot(a, b)).collect())
        .collect();
    let y = solve_equilibrated(jjt, rows.iter().map(|(_, c)| *c).collect())?;
    let n = jac.f.len();
    Some(
        (0..n)
            .map(|j| -rows.iter().zip(&y).map(|((r, _), y)| r[j] * y).sum::<f64>())
            .collect(),
    )
}

fn identity(n: usize, scale: f64) -> Vec<Vec<f64>> {
    (0..n)
        .map(|i| (0..n).map(|j| if i == j { scale } else { 0.0 }).collect())
        .collect()
}

/// Damped BFGS; `rescale` resets B to (yᵀy / sᵀy) I first.
fn bfgs(b: &mut [Vec<f64>], s: &[f64], y: &[f64], rescale: bool) {
    let dot = |a: &[f64], c: &[f64]| a.iter().zip(c).map(|(a, c)| a * c).sum::<f64>();
    let sy = dot(s, y);
    if rescale && sy > 0.0 {
        let k = dot(y, y) / sy;
        for (i, row) in b.iter_mut().enumerate() {
            row.iter_mut()
                .enumerate()
                .for_each(|(j, v)| *v = if i == j { k } else { 0.0 });
        }
    }
    let bs: Vec<f64> = b.iter().map(|row| dot(row, s)).collect();
    let sbs = dot(s, &bs);
    if sbs.is_nan() || sbs <= 0.0 {
        return;
    }
    let theta = if sy >= 0.2 * sbs {
        1.0
    } else {
        0.8 * sbs / (sbs - sy)
    };
    let r: Vec<f64> = y
        .iter()
        .zip(&bs)
        .map(|(y, bs)| theta * y + (1.0 - theta) * bs)
        .collect();
    let sr = dot(s, &r);
    if sr.is_nan() || sr <= 0.0 {
        return;
    }
    for (i, row) in b.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v += r[i] * r[j] / sr - bs[i] * bs[j] / sbs;
        }
    }
}

struct Step {
    d: Vec<f64>,
    lambda: Vec<f64>,
    mu: Vec<f64>,
    /// Σ|h + H d| + Σ max(0, g + G d).
    linear_violation: f64,
    /// Largest |λ|, μ; ≤ σ.
    multiplier: f64,
}

/// min ½dᵀBd + ∇fᵀd + σ Σ(v + w + t)
/// s.t. H d − v + w = −h, G d − t ≤ −g, lo ≤ d ≤ hi, v, w, t ≥ 0.
fn subproblem(
    b: &[Vec<f64>],
    jac: &Jacobian,
    at: &Point,
    box_: &[(f64, f64)],
    sigma: f64,
) -> Option<Step> {
    let n = b.len();
    let (me, mi) = (jac.h.len(), jac.g.len());
    let nv = n + 2 * me + mi;
    let mut q = vec![vec![0.0; nv]; nv];
    for i in 0..n {
        q[i][..n].copy_from_slice(&b[i]);
    }
    let mut c = vec![sigma; nv];
    c[..n].copy_from_slice(&jac.f);

    let mut a = vec![vec![0.0; nv]; me];
    let mut rhs = vec![0.0; me];
    for i in 0..me {
        a[i][..n].copy_from_slice(&jac.h[i]);
        a[i][n + i] = -1.0;
        a[i][n + me + i] = 1.0;
        rhs[i] = -at.h[i];
    }
    let mut ci = Vec::new();
    let mut e = Vec::new();
    for i in 0..mi {
        let mut row = vec![0.0; nv];
        row[..n].copy_from_slice(&jac.g[i]);
        row[n + 2 * me + i] = -1.0;
        ci.push(row);
        e.push(-at.g[i]);
    }
    for (j, &(lo, hi)) in box_.iter().enumerate() {
        let mut row = vec![0.0; nv];
        row[j] = 1.0;
        ci.push(row.clone());
        e.push(hi);
        row[j] = -1.0;
        ci.push(row);
        e.push(-lo);
    }
    for k in n..nv {
        let mut row = vec![0.0; nv];
        row[k] = -1.0;
        ci.push(row);
        e.push(0.0);
    }

    let (x, y, z) = interior_point(&q, &c, &a, &rhs, &ci, &e)?;
    let d = x[..n].to_vec();
    let dot = |r: &[f64]| r.iter().zip(&d).map(|(r, d)| r * d).sum::<f64>();
    let linear_violation = jac
        .h
        .iter()
        .zip(&at.h)
        .map(|(r, h)| (h + dot(r)).abs())
        .chain(jac.g.iter().zip(&at.g).map(|(r, g)| (g + dot(r)).max(0.0)))
        .sum();
    let mu = z[..mi].to_vec();
    let multiplier = y.iter().chain(&mu).fold(0.0_f64, |m, v| m.max(v.abs()));
    Some(Step {
        d,
        lambda: y,
        mu,
        linear_violation,
        multiplier,
    })
}

/// Newton direction (dx, dy, dz, ds).
type Direction = (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>);

/// Convex QP min ½xᵀQx + cᵀx s.t. Ax = b, Cx ≤ e, by Mehrotra predictor-corrector.
/// Returns (x, y, z): stationarity Qx + c + Aᵀy + Cᵀz = 0, z ≥ 0.
fn interior_point(
    q: &[Vec<f64>],
    c: &[f64],
    a: &[Vec<f64>],
    b: &[f64],
    ci: &[Vec<f64>],
    e: &[f64],
) -> Option<(Vec<f64>, Vec<f64>, Vec<f64>)> {
    let n = c.len();
    let (ma, mc) = (b.len(), e.len());
    let dot = |r: &[f64], x: &[f64]| r.iter().zip(x).map(|(r, x)| r * x).sum::<f64>();
    let inf = |v: &[f64]| v.iter().fold(0.0_f64, |m, x| m.max(x.abs()));
    let mut x = vec![0.0; n];
    let mut y = vec![0.0; ma];
    let mut s: Vec<f64> = ci
        .iter()
        .zip(e)
        .map(|(r, e)| (e - dot(r, &x)).max(1.0))
        .collect();
    let mut z = vec![1.0; mc];
    let mut best = (f64::INFINITY, None);

    for _ in 0..200 {
        let rd: Vec<f64> = (0..n)
            .map(|i| {
                dot(&q[i], &x)
                    + c[i]
                    + (0..ma).map(|k| a[k][i] * y[k]).sum::<f64>()
                    + (0..mc).map(|k| ci[k][i] * z[k]).sum::<f64>()
            })
            .collect();
        let rp: Vec<f64> = (0..ma).map(|k| dot(&a[k], &x) - b[k]).collect();
        let ri: Vec<f64> = (0..mc).map(|k| dot(&ci[k], &x) + s[k] - e[k]).collect();
        let gap = dot(&s, &z) / mc.max(1) as f64;
        // Residuals relative to the magnitudes they balance.
        let qx: Vec<f64> = q.iter().map(|r| dot(r, &x)).collect();
        let cx: Vec<f64> = ci.iter().map(|r| dot(r, &x)).collect();
        let dual_scale = 1.0 + inf(c).max(inf(&qx));
        let primal_scale = 1.0 + inf(b).max(inf(e)).max(inf(&cx));
        let objective = 0.5 * dot(&qx, &x) + dot(c, &x);
        let error = (inf(&rd) / dual_scale)
            .max(inf(&rp).max(inf(&ri)) / primal_scale)
            .max(gap / (1.0 + objective.abs()));
        if error < best.0 {
            best = (error, Some((x.clone(), y.clone(), z.clone())));
        }
        if error <= 1e-11 {
            break;
        }

        // [Q + CᵀWC, Aᵀ; A, 0]
        let w: Vec<f64> = z.iter().zip(&s).map(|(z, s)| z / s).collect();
        let mut kkt = vec![vec![0.0; n + ma]; n + ma];
        for i in 0..n {
            for j in 0..n {
                kkt[i][j] = q[i][j] + (0..mc).map(|k| ci[k][i] * w[k] * ci[k][j]).sum::<f64>();
            }
            for k in 0..ma {
                kkt[i][n + k] = a[k][i];
                kkt[n + k][i] = a[k][i];
            }
        }
        let solve = |rc: &[f64]| -> Option<Direction> {
            // dz = S⁻¹(rc + Z ri + Z C dx), ds = −ri − C dx.
            let t: Vec<f64> = (0..mc).map(|k| (rc[k] + z[k] * ri[k]) / s[k]).collect();
            let mut rhs: Vec<f64> = (0..n)
                .map(|i| -rd[i] - (0..mc).map(|k| ci[k][i] * t[k]).sum::<f64>())
                .collect();
            rhs.extend(rp.iter().map(|v| -v));
            let sol = solve_equilibrated(kkt.clone(), rhs)?;
            let dx = sol[..n].to_vec();
            let dy = sol[n..].to_vec();
            let cdx: Vec<f64> = ci.iter().map(|r| dot(r, &dx)).collect();
            let dz = (0..mc).map(|k| t[k] + w[k] * cdx[k]).collect();
            let ds = (0..mc).map(|k| -ri[k] - cdx[k]).collect();
            Some((dx, dy, dz, ds))
        };
        let to_boundary = |v: &[f64], dv: &[f64]| {
            v.iter()
                .zip(dv)
                .filter(|(_, d)| **d < 0.0)
                .fold(1.0_f64, |a, (v, d)| a.min(-v / d))
        };

        let affine: Vec<f64> = s.iter().zip(&z).map(|(s, z)| -s * z).collect();
        let Some((_, _, dz_a, ds_a)) = solve(&affine) else {
            break;
        };
        let (ap, ad) = (to_boundary(&s, &ds_a), to_boundary(&z, &dz_a));
        let gap_a = (0..mc)
            .map(|k| (s[k] + ap * ds_a[k]) * (z[k] + ad * dz_a[k]))
            .sum::<f64>()
            / mc.max(1) as f64;
        let centering = if gap > 0.0 {
            (gap_a / gap).powi(3)
        } else {
            0.0
        };
        let corrected: Vec<f64> = (0..mc)
            .map(|k| -s[k] * z[k] - ds_a[k] * dz_a[k] + centering * gap)
            .collect();
        let Some((dx, dy, dz, ds)) = solve(&corrected) else {
            break;
        };
        let ap = (0.99 * to_boundary(&s, &ds)).min(1.0);
        let ad = (0.99 * to_boundary(&z, &dz)).min(1.0);
        x.iter_mut().zip(&dx).for_each(|(v, d)| *v += ap * d);
        s.iter_mut().zip(&ds).for_each(|(v, d)| *v += ap * d);
        y.iter_mut().zip(&dy).for_each(|(v, d)| *v += ad * d);
        z.iter_mut().zip(&dz).for_each(|(v, d)| *v += ad * d);
    }
    if best.0 <= 1e-7 { best.1 } else { None }
}

/// Gaussian elimination after row equilibration, partial pivoting. IP KKT diagonals span
/// many decades, so pivots are only rejected when zero or non-finite.
fn solve_equilibrated(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for (row, b) in a.iter_mut().zip(&mut b) {
        let m = row.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        if !(m > 0.0 && m.is_finite()) {
            return None;
        }
        row.iter_mut().for_each(|v| *v /= m);
        *b /= m;
    }
    for c in 0..n {
        let p = (c..n).max_by(|&x, &y| a[x][c].abs().total_cmp(&a[y][c].abs()))?;
        if a[p][c] == 0.0 || !a[p][c].is_finite() {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in c + 1..n {
            let f = a[r][c] / a[c][c];
            if f == 0.0 {
                continue;
            }
            let (pivot, rest) = a.split_at_mut(r);
            for (x, p) in rest[0][c..].iter_mut().zip(&pivot[c][c..]) {
                *x -= f * p;
            }
            b[r] -= f * b[c];
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        x[r] = (b[r] - (r + 1..n).map(|k| a[r][k] * x[k]).sum::<f64>()) / a[r][r];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tol(eq: usize, ineq: usize) -> Tolerances {
        Tolerances {
            objective: 1e-9,
            equalities: vec![1e-9; eq],
            inequalities: vec![1e-9; ineq],
        }
    }

    fn var(value: f64) -> Variable {
        Variable::new(value, 1e-7, 1.0)
    }

    #[test]
    fn qp_with_active_inequality() {
        // min x² + y² s.t. x + y ≥ 1 → (½, ½), μ = 1.
        let q = vec![vec![2.0, 0.0], vec![0.0, 2.0]];
        let (x, _, z) =
            interior_point(&q, &[0.0, 0.0], &[], &[], &[vec![-1.0, -1.0]], &[-1.0]).unwrap();
        assert!(
            (x[0] - 0.5).abs() < 1e-8 && (x[1] - 0.5).abs() < 1e-8,
            "{x:?}"
        );
        assert!((z[0] - 1.0).abs() < 1e-8);
    }

    #[test]
    fn rosenbrock_unconstrained() {
        let sol = minimize(
            |x| {
                Ok::<_, ()>(Evaluation {
                    objective: 100.0 * (x[1] - x[0] * x[0]).powi(2) + (1.0 - x[0]).powi(2),
                    ..Default::default()
                })
            },
            &[var(-1.2), var(1.0)],
            &Tolerances {
                objective: 1e-9,
                equalities: vec![],
                inequalities: vec![],
            },
            200,
        )
        .unwrap();
        assert!(sol.converged, "{sol:?}");
        assert!((sol.values[0] - 1.0).abs() < 1e-3 && (sol.values[1] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn equality_on_circle() {
        // min x + y s.t. x² + y² = 2 → (−1, −1).
        let sol = minimize(
            |x| {
                Ok::<_, ()>(Evaluation {
                    objective: x[0] + x[1],
                    equalities: vec![x[0] * x[0] + x[1] * x[1] - 2.0],
                    inequalities: vec![],
                })
            },
            &[var(1.0), var(0.2)],
            &tol(1, 0),
            100,
        )
        .unwrap();
        assert!(sol.converged, "{sol:?}");
        assert!((sol.values[0] + 1.0).abs() < 1e-4 && (sol.values[1] + 1.0).abs() < 1e-4);
    }

    #[test]
    fn hock_schittkowski_71() {
        // f* = 17.0140173 at (1, 4.743, 3.8211, 1.3794).
        let sol = minimize(
            |x| {
                Ok::<_, ()>(Evaluation {
                    objective: x[0] * x[3] * (x[0] + x[1] + x[2]) + x[2],
                    equalities: vec![x.iter().map(|v| v * v).sum::<f64>() - 40.0],
                    inequalities: vec![25.0 - x.iter().product::<f64>()],
                })
            },
            &[1.0, 5.0, 5.0, 1.0].map(|v| Variable::new(v, 1e-7, 1.0).bounded(1.0, 5.0)),
            &Tolerances {
                objective: 1e-7,
                equalities: vec![1e-7],
                inequalities: vec![1e-7],
            },
            100,
        )
        .unwrap();
        assert!(sol.converged, "{sol:?}");
        assert!(
            (sol.evaluation.objective - 17.014_017_3).abs() < 1e-4,
            "{sol:?}"
        );
        assert!((sol.values[1] - 4.743).abs() < 1e-3);
    }

    #[test]
    fn bounds_hold_and_infeasible_start_recovers() {
        // min (x − 3)² s.t. x ≤ 2 bound, x ≥ 1.5 inequality; start infeasible at 0.
        let sol = minimize(
            |x| {
                Ok::<_, ()>(Evaluation {
                    objective: (x[0] - 3.0).powi(2),
                    equalities: vec![],
                    inequalities: vec![1.5 - x[0]],
                })
            },
            &[Variable::new(0.0, 1e-7, 0.4).bounded(f64::NEG_INFINITY, 2.0)],
            &tol(0, 1),
            50,
        )
        .unwrap();
        assert!(sol.converged, "{sol:?}");
        assert!((sol.values[0] - 2.0).abs() < 1e-9);
        assert!(sol.history.len() > 5, "max_step bounds each step");
    }

    #[test]
    fn errors_at_start_propagate() {
        assert_eq!(
            minimize(|_| Err::<Evaluation, _>("bad"), &[var(0.0)], &tol(0, 0), 5),
            Err("bad")
        );
    }
}
