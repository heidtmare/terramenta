//! Differential correction: Newton iteration on goal residuals.
//!
//! Variables and residuals are scaled by perturbation and tolerance. The Jacobian is
//! forward-difference. Steps are minimum-norm (fewer goals than variables) or least-squares
//! (more goals), clamped per variable to `max_step` by scaling the whole step, then halved
//! until the residual norm drops.

/// One varied parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Variable {
    pub value: f64,
    /// Forward-difference step; also the variable's scale.
    pub perturbation: f64,
    /// Largest change per iteration.
    pub max_step: f64,
}

/// Outcome of [`solve`].
#[derive(Debug, Clone, PartialEq)]
pub struct Solution {
    pub values: Vec<f64>,
    /// Residuals at `values`, unscaled.
    pub residuals: Vec<f64>,
    /// Every |residual| ≤ its tolerance.
    pub converged: bool,
    /// Newton steps taken.
    pub iterations: usize,
    /// Largest |residual / tolerance| before each step, then at `values`.
    pub history: Vec<f64>,
}

/// Step halvings before giving up on an iteration.
const MAX_HALVINGS: usize = 8;

/// Drives `residuals(x)` (achieved − desired, one per tolerance) within `tolerances`.
/// An `Err` from the initial point or a Jacobian column is returned; from a trial step it
/// counts as no improvement.
pub fn solve<E>(
    mut residuals: impl FnMut(&[f64]) -> Result<Vec<f64>, E>,
    variables: &[Variable],
    tolerances: &[f64],
    max_iterations: usize,
) -> Result<Solution, E> {
    let n = variables.len();
    let m = tolerances.len();
    let scaled = |r: &[f64]| -> Vec<f64> { r.iter().zip(tolerances).map(|(r, t)| r / t).collect() };
    let worst = |s: &[f64]| s.iter().fold(0.0_f64, |w, x| w.max(x.abs()));
    let norm = |s: &[f64]| s.iter().map(|x| x * x).sum::<f64>();

    let mut x: Vec<f64> = variables.iter().map(|v| v.value).collect();
    let mut r = residuals(&x)?;
    let mut s = scaled(&r);
    let mut history = vec![worst(&s)];
    let mut iterations = 0;

    while worst(&s) > 1.0 && iterations < max_iterations && n > 0 {
        // J[i][j] = ∂s_i / ∂(x_j / p_j).
        let mut jacobian = vec![vec![0.0; n]; m];
        for (j, v) in variables.iter().enumerate() {
            let mut probe = x.clone();
            probe[j] += v.perturbation;
            let sp = scaled(&residuals(&probe)?);
            for i in 0..m {
                jacobian[i][j] = sp[i] - s[i];
            }
        }
        let Some(dz) = newton_step(&jacobian, &s) else {
            break;
        };
        let mut dx: Vec<f64> = dz
            .iter()
            .zip(variables)
            .map(|(dz, v)| dz * v.perturbation)
            .collect();
        let over = dx
            .iter()
            .zip(variables)
            .fold(1.0_f64, |k, (d, v)| k.max(d.abs() / v.max_step));
        dx.iter_mut().for_each(|d| *d /= over);

        let mut accepted = None;
        let mut alpha = 1.0;
        for _ in 0..=MAX_HALVINGS {
            let trial: Vec<f64> = x.iter().zip(&dx).map(|(x, d)| x + alpha * d).collect();
            if let Ok(rt) = residuals(&trial) {
                let st = scaled(&rt);
                if st.iter().all(|v| v.is_finite()) && norm(&st) < norm(&s) {
                    accepted = Some((trial, rt, st));
                    break;
                }
            }
            alpha /= 2.0;
        }
        let Some((xt, rt, st)) = accepted else {
            break;
        };
        (x, r, s) = (xt, rt, st);
        iterations += 1;
        history.push(worst(&s));
    }

    Ok(Solution {
        converged: worst(&s) <= 1.0,
        values: x,
        residuals: r,
        iterations,
        history,
    })
}

/// −J⁺ s: `Jᵀ (J Jᵀ)⁻¹` when m ≤ n, `(Jᵀ J)⁻¹ Jᵀ` otherwise. `None` when singular.
fn newton_step(j: &[Vec<f64>], s: &[f64]) -> Option<Vec<f64>> {
    let m = j.len();
    let n = j.first().map_or(0, Vec::len);
    if m <= n {
        let a: Vec<Vec<f64>> = (0..m)
            .map(|i| (0..m).map(|k| dot(&j[i], &j[k])).collect())
            .collect();
        let y = solve_linear(a, s.to_vec())?;
        Some(
            (0..n)
                .map(|c| -(0..m).map(|i| j[i][c] * y[i]).sum::<f64>())
                .collect(),
        )
    } else {
        let col = |c: usize| j.iter().map(move |row| row[c]);
        let a: Vec<Vec<f64>> = (0..n)
            .map(|c| {
                (0..n)
                    .map(|k| col(c).zip(col(k)).map(|(a, b)| a * b).sum())
                    .collect()
            })
            .collect();
        let b: Vec<f64> = (0..n)
            .map(|c| col(c).zip(s).map(|(a, b)| a * b).sum())
            .collect();
        Some(solve_linear(a, b)?.into_iter().map(|v| -v).collect())
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

/// Gaussian elimination, partial pivoting. `None` when a pivot is negligible.
fn solve_linear(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    let scale = a.iter().flatten().fold(0.0_f64, |m, x| m.max(x.abs()));
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    for c in 0..n {
        let p = (c..n).max_by(|&x, &y| a[x][c].abs().total_cmp(&a[y][c].abs()))?;
        if a[p][c].abs() <= scale * 1e-14 {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in c + 1..n {
            let f = a[r][c] / a[c][c];
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
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(value: f64) -> Variable {
        Variable {
            value,
            perturbation: 1e-6,
            max_step: 10.0,
        }
    }

    #[test]
    fn linear_square_in_one_step() {
        let sol = solve(
            |x| Ok::<_, ()>(vec![2.0 * x[0] + x[1] - 5.0, x[0] - x[1] - 1.0]),
            &[var(0.0), var(0.0)],
            &[1e-6, 1e-6],
            10,
        )
        .unwrap();
        assert!(sol.converged);
        assert_eq!(sol.iterations, 1);
        assert!((sol.values[0] - 2.0).abs() < 1e-6 && (sol.values[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn nonlinear_square() {
        let sol = solve(
            |x| Ok::<_, ()>(vec![x[0] * x[0] + x[1] * x[1] - 4.0, x[0] - x[1]]),
            &[var(1.0), var(0.5)],
            &[1e-10, 1e-10],
            20,
        )
        .unwrap();
        assert!(sol.converged, "{sol:?}");
        let root = 2.0_f64.sqrt();
        assert!((sol.values[0] - root).abs() < 1e-8 && (sol.values[1] - root).abs() < 1e-8);
        assert!(sol.history.windows(2).all(|w| w[1] < w[0]));
    }

    #[test]
    fn underdetermined_takes_minimum_norm() {
        let sol = solve(
            |x| Ok::<_, ()>(vec![x[0] + x[1] - 2.0]),
            &[var(0.0), var(0.0)],
            &[1e-9],
            5,
        )
        .unwrap();
        assert!((sol.values[0] - 1.0).abs() < 1e-6 && (sol.values[1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn overdetermined_least_squares_does_not_converge() {
        // x = 0 and x = 2: best fit 1, residual 1 each, tolerance 0.1.
        let sol = solve(
            |x| Ok::<_, ()>(vec![x[0], x[0] - 2.0]),
            &[var(5.0)],
            &[0.1, 0.1],
            10,
        )
        .unwrap();
        assert!(!sol.converged);
        assert!((sol.values[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn steps_clamped_and_errors_backed_off() {
        // Undefined beyond x = 3; a full Newton step from 0 lands at 10.
        let sol = solve(
            |x| {
                if x[0] > 3.0 {
                    Err("out of range")
                } else {
                    Ok(vec![x[0] - 2.0])
                }
            },
            &[Variable {
                value: 0.0,
                perturbation: 1e-6,
                max_step: 8.0,
            }],
            &[1e-9],
            20,
        )
        .unwrap();
        assert!(sol.converged);
        assert!((sol.values[0] - 2.0).abs() < 1e-8);
        assert_eq!(
            solve(|_| Err::<Vec<f64>, _>("bad"), &[var(0.0)], &[1.0], 5),
            Err("bad")
        );
    }
}
