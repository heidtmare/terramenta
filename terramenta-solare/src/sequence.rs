//! Mission sequence: ordered propagate / maneuver / finite-burn / target commands run on one
//! spacecraft.
//!
//! - `Propagate` stops at an epoch, after a duration, or at the `count`-th crossing of an
//!   osculating [`Quantity`] through a value (apsides: radial velocity through 0; nodes:
//!   argument of latitude through 0°/180°). Crossings are bracketed every `step_s` and
//!   refined to [`CROSSING_TOLERANCE_S`] (Illinois); the stop state is propagated, not
//!   interpolated. Crossings within [`CROSSING_GUARD_S`] of the segment start are ignored.
//! - `Maneuver` applies an impulse at the current epoch.
//! - `FiniteBurn` thrusts from the current epoch for its duration.
//! - `Target` varies labelled commands in its body ([`Parameter`]) by differential correction
//!   ([`crate::targeting::solve`]) until goals at the body's end are met, then runs the body
//!   with the solution.
//! - `Optimize` varies labelled body commands within bounds to minimize total Δv or a
//!   quantity at the body's end, subject to equality and inequality constraints there
//!   ([`crate::optimize::minimize`]), then runs the body with the solution.
//!
//! Forward only. Quantities are two-body about Earth (EGM2008 GM), GCRF.

use glam::DVec3;

use crate::force::EGM2008_GM_KM3_S2;
use crate::maneuver::{BurnFrame, FiniteBurn, ImpulsiveManeuver, LocalFrame};
use crate::optimize::{self, Evaluation, Progress, Tolerances};
use crate::propagate::{PropagationError, Propagator, SpacecraftState};
use crate::targeting::{self, Variable};
use crate::time::Epoch;

/// Standard gravity for finite-burn Δv, m/s².
const G0_M_S2: f64 = 9.806_65;
/// Earth equatorial radius for altitudes, km.
const EARTH_RADIUS_KM: f64 = 6378.137;
/// Crossing root tolerance, s.
pub const CROSSING_TOLERANCE_S: f64 = 1e-4;
/// Crossings this close to a segment start are the start itself, s.
pub const CROSSING_GUARD_S: f64 = 1e-2;

/// Osculating quantity of a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantity {
    AltitudeKm,
    RadiusKm,
    SpeedKmS,
    RadialVelocityKmS,
    SmaKm,
    Eccentricity,
    InclinationDeg,
    RaanDeg,
    ArgPerigeeDeg,
    TrueAnomalyDeg,
    ArgLatitudeDeg,
    /// Geocentric.
    LatitudeDeg,
    PeriodMin,
    PerigeeAltKm,
    ApogeeAltKm,
    FlightPathAngleDeg,
    MassKg,
}

impl Quantity {
    pub const ALL: [Quantity; 17] = [
        Self::AltitudeKm,
        Self::RadiusKm,
        Self::SpeedKmS,
        Self::RadialVelocityKmS,
        Self::SmaKm,
        Self::Eccentricity,
        Self::InclinationDeg,
        Self::RaanDeg,
        Self::ArgPerigeeDeg,
        Self::TrueAnomalyDeg,
        Self::ArgLatitudeDeg,
        Self::LatitudeDeg,
        Self::PeriodMin,
        Self::PerigeeAltKm,
        Self::ApogeeAltKm,
        Self::FlightPathAngleDeg,
        Self::MassKg,
    ];

    /// camelCase name, e.g. `smaKm`.
    pub fn name(self) -> &'static str {
        match self {
            Self::AltitudeKm => "altitudeKm",
            Self::RadiusKm => "radiusKm",
            Self::SpeedKmS => "speedKmS",
            Self::RadialVelocityKmS => "radialVelocityKmS",
            Self::SmaKm => "smaKm",
            Self::Eccentricity => "eccentricity",
            Self::InclinationDeg => "inclinationDeg",
            Self::RaanDeg => "raanDeg",
            Self::ArgPerigeeDeg => "argPerigeeDeg",
            Self::TrueAnomalyDeg => "trueAnomalyDeg",
            Self::ArgLatitudeDeg => "argLatitudeDeg",
            Self::LatitudeDeg => "latitudeDeg",
            Self::PeriodMin => "periodMin",
            Self::PerigeeAltKm => "perigeeAltKm",
            Self::ApogeeAltKm => "apogeeAltKm",
            Self::FlightPathAngleDeg => "flightPathAngleDeg",
            Self::MassKg => "massKg",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|q| q.name() == name)
    }

    /// Wraps at 360°.
    pub fn is_angle(self) -> bool {
        matches!(
            self,
            Self::RaanDeg | Self::ArgPerigeeDeg | Self::TrueAnomalyDeg | Self::ArgLatitudeDeg
        )
    }

    pub fn of(self, s: &SpacecraftState) -> f64 {
        let (r, v) = (s.position_km, s.velocity_km_s);
        let mu = EGM2008_GM_KM3_S2;
        let (radius, speed) = (r.length(), v.length());
        let h = r.cross(v);
        let rv = r.dot(v);
        let e_vec = ((speed * speed - mu / radius) * r - rv * v) / mu;
        let e = e_vec.length();
        let a = -mu / (2.0 * (speed * speed / 2.0 - mu / radius));
        let node = DVec3::new(-h.y, h.x, 0.0);
        let wrap = |x: f64| x.rem_euclid(360.0);
        let angle = |a: DVec3, b: DVec3| {
            (a.dot(b) / (a.length() * b.length()))
                .clamp(-1.0, 1.0)
                .acos()
                .to_degrees()
        };
        match self {
            Self::AltitudeKm => radius - EARTH_RADIUS_KM,
            Self::RadiusKm => radius,
            Self::SpeedKmS => speed,
            Self::RadialVelocityKmS => rv / radius,
            Self::SmaKm => a,
            Self::Eccentricity => e,
            Self::InclinationDeg => (h.z / h.length()).clamp(-1.0, 1.0).acos().to_degrees(),
            Self::RaanDeg => wrap(node.y.atan2(node.x).to_degrees()),
            Self::ArgPerigeeDeg => {
                let w = angle(node, e_vec);
                wrap(if e_vec.z < 0.0 { -w } else { w })
            }
            Self::TrueAnomalyDeg => {
                let nu = angle(e_vec, r);
                if rv < 0.0 { 360.0 - nu } else { nu }
            }
            Self::ArgLatitudeDeg => {
                let u = angle(node, r);
                if r.z < 0.0 { 360.0 - u } else { u }
            }
            Self::LatitudeDeg => (r.z / radius).clamp(-1.0, 1.0).asin().to_degrees(),
            Self::PeriodMin => 2.0 * std::f64::consts::PI * (a.powi(3) / mu).sqrt() / 60.0,
            Self::PerigeeAltKm => a * (1.0 - e) - EARTH_RADIUS_KM,
            Self::ApogeeAltKm => a * (1.0 + e) - EARTH_RADIUS_KM,
            Self::FlightPathAngleDeg => {
                (rv / (radius * speed)).clamp(-1.0, 1.0).asin().to_degrees()
            }
            Self::MassKg => s.mass_kg,
        }
    }

    /// `achieved − desired`; angles wrapped to ±180°.
    pub fn error(self, s: &SpacecraftState, desired: f64) -> f64 {
        let e = self.of(s) - desired;
        if self.is_angle() {
            (e + 180.0).rem_euclid(360.0) - 180.0
        } else {
            e
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    Either,
    Increasing,
    Decreasing,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stop {
    Epoch(Epoch),
    Duration(f64),
    Crossing {
        quantity: Quantity,
        value: f64,
        direction: Direction,
    },
}

impl Stop {
    pub fn periapsis() -> Self {
        Self::crossing(Quantity::RadialVelocityKmS, 0.0, Direction::Increasing)
    }

    pub fn apoapsis() -> Self {
        Self::crossing(Quantity::RadialVelocityKmS, 0.0, Direction::Decreasing)
    }

    pub fn ascending_node() -> Self {
        Self::crossing(Quantity::ArgLatitudeDeg, 0.0, Direction::Increasing)
    }

    pub fn descending_node() -> Self {
        Self::crossing(Quantity::ArgLatitudeDeg, 180.0, Direction::Increasing)
    }

    pub fn crossing(quantity: Quantity, value: f64, direction: Direction) -> Self {
        Self::Crossing {
            quantity,
            value,
            direction,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Propagate {
        stop: Stop,
        /// Crossing occurrence, ≥ 1.
        count: usize,
        /// Bracketing step, s.
        step_s: f64,
        /// Crossing search limit, s.
        max_duration_s: f64,
    },
    Maneuver {
        delta_v_km_s: DVec3,
        frame: LocalFrame,
        isp_s: Option<f64>,
    },
    FiniteBurn {
        duration_s: f64,
        thrust_n: f64,
        isp_s: f64,
        /// Unit vector in `frame`.
        direction: DVec3,
        frame: BurnFrame,
    },
    Target(Box<Target>),
    Optimize(Box<Optimize>),
}

impl Action {
    /// Nested commands of a target or optimize.
    fn body(&self) -> Option<&[Command]> {
        match self {
            Self::Target(t) => Some(&t.body),
            Self::Optimize(o) => Some(&o.body),
            _ => None,
        }
    }

    fn body_mut(&mut self) -> Option<&mut Vec<Command>> {
        match self {
            Self::Target(t) => Some(&mut t.body),
            Self::Optimize(o) => Some(&mut o.body),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub label: Option<String>,
    pub action: Action,
}

/// What a target variable changes on its labelled command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parameter {
    /// Maneuver Δv component (0–2) in its frame, km/s.
    DeltaV(usize),
    /// Finite burn duration, or a `Stop::Duration` propagate, s.
    Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetVariable {
    pub command: String,
    pub parameter: Parameter,
    pub perturbation: f64,
    pub max_step: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Goal {
    pub quantity: Quantity,
    pub value: f64,
    pub tolerance: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub variables: Vec<TargetVariable>,
    /// At the end of `body`.
    pub goals: Vec<Goal>,
    pub body: Vec<Command>,
    pub max_iterations: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OptimizeVariable {
    pub command: String,
    pub parameter: Parameter,
    pub perturbation: f64,
    pub max_step: f64,
    /// ±∞ when unbounded.
    pub lower: f64,
    pub upper: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Objective {
    /// Σ impulsive |Δv| + Σ finite-burn g₀ Isp ln(m₀/m₁) over the body, km/s.
    DeltaV,
    /// At the end of the body.
    Quantity { quantity: Quantity, maximize: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Equal,
    AtLeast,
    AtMost,
}

impl Relation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::AtLeast => "atLeast",
            Self::AtMost => "atMost",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Equal, Self::AtLeast, Self::AtMost]
            .into_iter()
            .find(|r| r.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Constraint {
    pub quantity: Quantity,
    pub relation: Relation,
    pub value: f64,
    pub tolerance: f64,
}

impl Constraint {
    /// Equal: [`Quantity::error`]; inequalities: amount violated, ≤ 0 when satisfied.
    pub fn error(&self, s: &SpacecraftState) -> f64 {
        let e = self.quantity.error(s, self.value);
        match self.relation {
            Relation::Equal => e,
            Relation::AtLeast => -e,
            Relation::AtMost => e,
        }
    }

    /// Within tolerance.
    pub fn met(&self, error: f64) -> bool {
        match self.relation {
            Relation::Equal => error.abs() <= self.tolerance,
            _ => error <= self.tolerance,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Optimize {
    pub variables: Vec<OptimizeVariable>,
    pub objective: Objective,
    /// Objective units.
    pub objective_tolerance: f64,
    /// At the end of `body`.
    pub constraints: Vec<Constraint>,
    pub body: Vec<Command>,
    pub max_iterations: usize,
}

/// One executed command, in run order; a target's body follows its own entry at `depth + 1`.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub label: Option<String>,
    pub kind: SegmentKind,
    pub depth: usize,
    pub start: SpacecraftState,
    pub end: SpacecraftState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SegmentKind {
    Propagate,
    /// Inertial Δv.
    Maneuver(ImpulsiveManeuver, DVec3),
    FiniteBurn(FiniteBurn),
    Target(TargetOutcome),
    Optimize(OptimizeOutcome),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OptimizeOutcome {
    /// Feasible and stationary.
    pub converged: bool,
    pub feasible: bool,
    pub iterations: usize,
    /// Body runs.
    pub evaluations: usize,
    /// Objective units.
    pub objective: f64,
    pub history: Vec<Progress>,
    /// Variable order.
    pub values: Vec<f64>,
    /// Constraint order.
    pub constraints: Vec<ConstraintOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConstraintOutcome {
    pub constraint: Constraint,
    pub achieved: f64,
    /// [`Constraint::error`].
    pub error: f64,
    pub met: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetOutcome {
    pub converged: bool,
    pub iterations: usize,
    pub history: Vec<f64>,
    /// Variable order.
    pub values: Vec<f64>,
    /// Goal order.
    pub goals: Vec<GoalOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GoalOutcome {
    pub quantity: Quantity,
    pub value: f64,
    pub achieved: f64,
    /// [`Quantity::error`].
    pub error: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub segments: Vec<Segment>,
    pub maneuvers: Vec<ImpulsiveManeuver>,
    pub finite_burns: Vec<FiniteBurn>,
    pub end: SpacecraftState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SequenceError {
    Propagation(String),
    /// Crossing not found within `max_duration_s`.
    NotReached(String),
    Invalid(String),
}

impl std::fmt::Display for SequenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Propagation(e) | Self::NotReached(e) | Self::Invalid(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for SequenceError {}

impl From<PropagationError> for SequenceError {
    fn from(e: PropagationError) -> Self {
        Self::Propagation(e.to_string())
    }
}

/// Runs `commands` from `state` with `propagator` (no impulsive maneuvers of its own; finite
/// burns are added as they run).
pub fn run(
    propagator: &Propagator,
    state: SpacecraftState,
    commands: &[Command],
) -> Result<Run, SequenceError> {
    run_with(propagator, state, commands, &mut |_| {})
}

/// [`run`], with `execution` applied to each maneuver and finite burn as flown (after
/// targeting; target iterations fly them unmodified). Execution errors.
pub fn run_with(
    propagator: &Propagator,
    state: SpacecraftState,
    commands: &[Command],
    execution: &mut dyn FnMut(&mut Action),
) -> Result<Run, SequenceError> {
    validate(commands)?;
    let mut out = Run {
        segments: Vec::new(),
        maneuvers: Vec::new(),
        finite_burns: Vec::new(),
        end: state,
    };
    let mut p = propagator.clone();
    execute(&mut p, commands, 0, &mut out, execution)?;
    Ok(out)
}

/// `commands` with each target and optimize replaced by its body at `run`'s solution: the
/// nominal flown open loop. `run` is a run of `commands`.
pub fn freeze(commands: &[Command], run: &Run) -> Vec<Command> {
    let mut outcomes = run.segments.iter().filter_map(|s| match &s.kind {
        SegmentKind::Target(t) => Some(&t.values),
        SegmentKind::Optimize(o) => Some(&o.values),
        _ => None,
    });
    let mut out = Vec::new();
    flatten(commands, &mut outcomes, &mut out);
    out
}

/// Preorder, as [`execute`] records targets and optimizes.
fn flatten<'a>(
    commands: &[Command],
    values: &mut impl Iterator<Item = &'a Vec<f64>>,
    out: &mut Vec<Command>,
) {
    for c in commands {
        match &c.action {
            Action::Target(t) => {
                let v = values.next().expect("run of these commands");
                flatten(&target_values(t, v), values, out);
            }
            Action::Optimize(o) => {
                let v = values.next().expect("run of these commands");
                flatten(&optimize_values(o, v), values, out);
            }
            _ => out.push(c.clone()),
        }
    }
}

fn validate(commands: &[Command]) -> Result<(), SequenceError> {
    let bad = |m: String| Err(SequenceError::Invalid(m));
    for c in commands {
        let name = c.label.as_deref().unwrap_or("command");
        match &c.action {
            Action::Propagate {
                stop,
                count,
                step_s,
                max_duration_s,
            } => {
                let ok = match stop {
                    Stop::Epoch(_) => true,
                    Stop::Duration(d) => d.is_finite() && *d >= 0.0,
                    Stop::Crossing { value, .. } => {
                        value.is_finite()
                            && *count >= 1
                            && step_s.is_finite()
                            && *step_s > 0.0
                            && max_duration_s.is_finite()
                            && *max_duration_s > 0.0
                    }
                };
                if !ok {
                    return bad(format!("{name}: bad propagate stop"));
                }
            }
            Action::Maneuver {
                delta_v_km_s,
                isp_s,
                ..
            } => {
                if !delta_v_km_s.is_finite() || isp_s.is_some_and(|i| i.is_nan() || i <= 0.0) {
                    return bad(format!("{name}: bad maneuver"));
                }
            }
            Action::FiniteBurn {
                duration_s,
                thrust_n,
                isp_s,
                direction,
                ..
            } => {
                let pos = |x: f64| x.is_finite() && x > 0.0;
                let ok = duration_s.is_finite()
                    && *duration_s >= 0.0
                    && pos(*thrust_n)
                    && pos(*isp_s)
                    && pos(direction.length());
                if !ok {
                    return bad(format!(
                        "{name}: finite burn needs nonzero direction, positive thrust and isp"
                    ));
                }
            }
            Action::Target(t) => {
                if t.variables.is_empty() || t.goals.is_empty() {
                    return bad(format!("{name}: target needs variables and goals"));
                }
                for v in &t.variables {
                    validate_variable(name, &t.body, &v.command, v.parameter)?;
                    if !(v.perturbation > 0.0 && v.max_step > 0.0) {
                        return bad(format!("{name}: perturbation and maxStep must be positive"));
                    }
                }
                if t.goals
                    .iter()
                    .any(|g| !g.value.is_finite() || g.tolerance.is_nan() || g.tolerance <= 0.0)
                {
                    return bad(format!(
                        "{name}: goals need finite values and positive tolerances"
                    ));
                }
                validate(&t.body)?;
            }
            Action::Optimize(o) => {
                if o.variables.is_empty() {
                    return bad(format!("{name}: optimize needs variables"));
                }
                for v in &o.variables {
                    validate_variable(name, &o.body, &v.command, v.parameter)?;
                    if !(v.perturbation > 0.0 && v.max_step > 0.0) {
                        return bad(format!("{name}: perturbation and maxStep must be positive"));
                    }
                    if v.lower.is_nan() || v.upper.is_nan() || v.lower > v.upper {
                        return bad(format!("{name}: {:?} bounds out of order", v.command));
                    }
                }
                if o.objective_tolerance.is_nan() || o.objective_tolerance <= 0.0 {
                    return bad(format!("{name}: objective tolerance must be positive"));
                }
                if o.constraints
                    .iter()
                    .any(|c| !c.value.is_finite() || c.tolerance.is_nan() || c.tolerance <= 0.0)
                {
                    return bad(format!(
                        "{name}: constraints need finite values and positive tolerances"
                    ));
                }
                validate(&o.body)?;
            }
        }
    }
    Ok(())
}

/// `command` exists in `body` and has `parameter`.
fn validate_variable(
    name: &str,
    body: &[Command],
    command: &str,
    parameter: Parameter,
) -> Result<(), SequenceError> {
    let target = find(body, command).ok_or_else(|| {
        SequenceError::Invalid(format!("{name}: no command {command:?} in the body"))
    })?;
    let fits = matches!(
        (&target.action, parameter),
        (Action::Maneuver { .. }, Parameter::DeltaV(0..=2))
            | (Action::FiniteBurn { .. }, Parameter::Duration)
            | (
                Action::Propagate {
                    stop: Stop::Duration(_),
                    ..
                },
                Parameter::Duration
            )
    );
    if fits {
        Ok(())
    } else {
        Err(SequenceError::Invalid(format!(
            "{name}: {command:?} has no {parameter:?} parameter"
        )))
    }
}

/// First command labelled `label`, depth first.
fn find<'a>(commands: &'a [Command], label: &str) -> Option<&'a Command> {
    commands.iter().find_map(|c| {
        if c.label.as_deref() == Some(label) {
            return Some(c);
        }
        c.action.body().and_then(|b| find(b, label))
    })
}

fn find_mut<'a>(commands: &'a mut [Command], label: &str) -> Option<&'a mut Command> {
    for c in commands {
        if c.label.as_deref() == Some(label) {
            return Some(c);
        }
        if let Some(body) = c.action.body_mut()
            && let Some(found) = find_mut(body, label)
        {
            return Some(found);
        }
    }
    None
}

fn parameter(c: &Command, p: Parameter) -> f64 {
    match (&c.action, p) {
        (Action::Maneuver { delta_v_km_s, .. }, Parameter::DeltaV(i)) => delta_v_km_s[i],
        (Action::FiniteBurn { duration_s, .. }, Parameter::Duration) => *duration_s,
        (
            Action::Propagate {
                stop: Stop::Duration(d),
                ..
            },
            Parameter::Duration,
        ) => *d,
        _ => unreachable!("validated"),
    }
}

fn set_parameter(c: &mut Command, p: Parameter, value: f64) {
    match (&mut c.action, p) {
        (Action::Maneuver { delta_v_km_s, .. }, Parameter::DeltaV(i)) => delta_v_km_s[i] = value,
        (Action::FiniteBurn { duration_s, .. }, Parameter::Duration) => {
            *duration_s = value.max(0.0)
        }
        (
            Action::Propagate {
                stop: Stop::Duration(d),
                ..
            },
            Parameter::Duration,
        ) => *d = value.max(0.0),
        _ => unreachable!("validated"),
    }
}

/// `body` with `(command, parameter)` values applied.
fn with_values<'a>(
    body: &[Command],
    variables: impl Iterator<Item = (&'a str, Parameter)>,
    values: &[f64],
) -> Vec<Command> {
    let mut body = body.to_vec();
    for ((command, parameter), &x) in variables.zip(values) {
        let c = find_mut(&mut body, command).expect("validated");
        set_parameter(c, parameter, x);
    }
    body
}

fn target_values(t: &Target, values: &[f64]) -> Vec<Command> {
    let vars = t
        .variables
        .iter()
        .map(|v| (v.command.as_str(), v.parameter));
    with_values(&t.body, vars, values)
}

fn optimize_values(o: &Optimize, values: &[f64]) -> Vec<Command> {
    let vars = o
        .variables
        .iter()
        .map(|v| (v.command.as_str(), v.parameter));
    with_values(&o.body, vars, values)
}

/// Runs `body` from `start` alone.
fn trial(p: &Propagator, start: &SpacecraftState, body: &[Command]) -> Result<Run, SequenceError> {
    let mut out = Run {
        segments: Vec::new(),
        maneuvers: Vec::new(),
        finite_burns: Vec::new(),
        end: *start,
    };
    execute(&mut p.clone(), body, 0, &mut out, &mut |_| {})?;
    Ok(out)
}

/// Σ impulsive |Δv| + Σ finite-burn g₀ Isp ln(m₀/m₁), km/s.
pub fn delta_v_km_s(segments: &[Segment]) -> f64 {
    segments
        .iter()
        .map(|s| match &s.kind {
            SegmentKind::Maneuver(_, dv) => dv.length(),
            SegmentKind::FiniteBurn(b) if s.end.mass_kg > 0.0 => {
                G0_M_S2 * 1e-3 * b.isp_s * (s.start.mass_kg / s.end.mass_kg).ln()
            }
            _ => 0.0,
        })
        .sum()
}

fn execute(
    p: &mut Propagator,
    commands: &[Command],
    depth: usize,
    out: &mut Run,
    execution: &mut dyn FnMut(&mut Action),
) -> Result<(), SequenceError> {
    for c in commands {
        let start = out.end;
        let flown;
        let action = match &c.action {
            a @ (Action::Maneuver { .. } | Action::FiniteBurn { .. }) => {
                let mut a = a.clone();
                execution(&mut a);
                flown = a;
                &flown
            }
            a => a,
        };
        let kind = match action {
            Action::Propagate {
                stop,
                count,
                step_s,
                max_duration_s,
            } => {
                out.end = propagate_until(p, &start, *stop, *count, *step_s, *max_duration_s)?;
                SegmentKind::Propagate
            }
            Action::Maneuver {
                delta_v_km_s,
                frame,
                isp_s,
            } => {
                let mut m = ImpulsiveManeuver::new(start.epoch, *delta_v_km_s, *frame);
                m.isp_s = *isp_s;
                let (dv, mass) = m.apply(start.position_km, start.velocity_km_s, start.mass_kg);
                out.end.velocity_km_s += dv;
                out.end.mass_kg = mass;
                out.maneuvers.push(m);
                SegmentKind::Maneuver(m, dv)
            }
            Action::FiniteBurn {
                duration_s,
                thrust_n,
                isp_s,
                direction,
                frame,
            } => {
                let burn = FiniteBurn {
                    start: start.epoch,
                    duration_s: *duration_s,
                    thrust_n: *thrust_n,
                    isp_s: *isp_s,
                    direction: direction.normalize(),
                    frame: *frame,
                };
                if *duration_s > 0.0 {
                    *p = p.clone().with_force(burn);
                    out.end = p.propagate(&start, burn.end())?;
                    out.finite_burns.push(burn);
                }
                SegmentKind::FiniteBurn(burn)
            }
            Action::Target(t) => {
                let index = out.segments.len();
                out.segments.push(Segment {
                    label: c.label.clone(),
                    kind: SegmentKind::Propagate,
                    depth,
                    start,
                    end: start,
                });
                let outcome = solve(p, &start, t)?;
                execute(
                    p,
                    &target_values(t, &outcome.values),
                    depth + 1,
                    out,
                    execution,
                )?;
                let segment = &mut out.segments[index];
                segment.kind = SegmentKind::Target(outcome);
                segment.end = out.end;
                continue;
            }
            Action::Optimize(o) => {
                let index = out.segments.len();
                out.segments.push(Segment {
                    label: c.label.clone(),
                    kind: SegmentKind::Propagate,
                    depth,
                    start,
                    end: start,
                });
                let outcome = optimize_body(p, &start, o)?;
                execute(
                    p,
                    &optimize_values(o, &outcome.values),
                    depth + 1,
                    out,
                    execution,
                )?;
                let segment = &mut out.segments[index];
                segment.kind = SegmentKind::Optimize(outcome);
                segment.end = out.end;
                continue;
            }
        };
        out.segments.push(Segment {
            label: c.label.clone(),
            kind,
            depth,
            start,
            end: out.end,
        });
    }
    Ok(())
}

fn solve(
    p: &Propagator,
    start: &SpacecraftState,
    t: &Target,
) -> Result<TargetOutcome, SequenceError> {
    let variables: Vec<Variable> = t
        .variables
        .iter()
        .map(|v| Variable {
            value: parameter(find(&t.body, &v.command).expect("validated"), v.parameter),
            perturbation: v.perturbation,
            max_step: v.max_step,
        })
        .collect();
    let tolerances: Vec<f64> = t.goals.iter().map(|g| g.tolerance).collect();
    let end_of = |x: &[f64]| trial(p, start, &target_values(t, x)).map(|r| r.end);
    let errors = |s: &SpacecraftState| -> Vec<f64> {
        t.goals
            .iter()
            .map(|g| g.quantity.error(s, g.value))
            .collect()
    };
    let solution = targeting::solve(
        |x| end_of(x).map(|s| errors(&s)),
        &variables,
        &tolerances,
        t.max_iterations,
    )?;
    let end = end_of(&solution.values)?;
    Ok(TargetOutcome {
        converged: solution.converged,
        iterations: solution.iterations,
        history: solution.history,
        goals: t
            .goals
            .iter()
            .map(|g| GoalOutcome {
                quantity: g.quantity,
                value: g.value,
                achieved: g.quantity.of(&end),
                error: g.quantity.error(&end, g.value),
            })
            .collect(),
        values: solution.values,
    })
}

fn optimize_body(
    p: &Propagator,
    start: &SpacecraftState,
    o: &Optimize,
) -> Result<OptimizeOutcome, SequenceError> {
    let variables: Vec<optimize::Variable> = o
        .variables
        .iter()
        .map(|v| {
            let value = parameter(find(&o.body, &v.command).expect("validated"), v.parameter);
            optimize::Variable::new(value, v.perturbation, v.max_step).bounded(v.lower, v.upper)
        })
        .collect();
    let (equal, unequal): (Vec<&Constraint>, Vec<&Constraint>) = o
        .constraints
        .iter()
        .partition(|c| c.relation == Relation::Equal);
    let tolerances = Tolerances {
        objective: o.objective_tolerance,
        equalities: equal.iter().map(|c| c.tolerance).collect(),
        inequalities: unequal.iter().map(|c| c.tolerance).collect(),
    };
    let objective = |run: &Run| match o.objective {
        Objective::DeltaV => delta_v_km_s(&run.segments),
        Objective::Quantity { quantity, maximize } => {
            let v = quantity.of(&run.end);
            if maximize { -v } else { v }
        }
    };
    let solution = optimize::minimize(
        |x| {
            let run = trial(p, start, &optimize_values(o, x))?;
            Ok::<_, SequenceError>(Evaluation {
                objective: objective(&run),
                equalities: equal.iter().map(|c| c.error(&run.end)).collect(),
                inequalities: unequal.iter().map(|c| c.error(&run.end)).collect(),
            })
        },
        &variables,
        &tolerances,
        o.max_iterations,
    )?;
    let run = trial(p, start, &optimize_values(o, &solution.values))?;
    let sign = match o.objective {
        Objective::Quantity { maximize: true, .. } => -1.0,
        _ => 1.0,
    };
    Ok(OptimizeOutcome {
        converged: solution.converged,
        feasible: solution.feasible,
        iterations: solution.iterations,
        evaluations: solution.evaluations,
        objective: sign * solution.evaluation.objective,
        history: solution
            .history
            .iter()
            .map(|h| Progress {
                objective: sign * h.objective,
                ..*h
            })
            .collect(),
        constraints: o
            .constraints
            .iter()
            .map(|c| {
                let error = c.error(&run.end);
                ConstraintOutcome {
                    constraint: *c,
                    achieved: c.quantity.of(&run.end),
                    error,
                    met: c.met(error),
                }
            })
            .collect(),
        values: solution.values,
    })
}

/// Sign change of `achieved − value` from `a` to `b`; angle wraps are not crossings.
fn crosses(direction: Direction, a: f64, b: f64, angle: bool) -> bool {
    if angle && (b - a).abs() > 180.0 {
        return false;
    }
    match direction {
        Direction::Increasing => a < 0.0 && b >= 0.0,
        Direction::Decreasing => a > 0.0 && b <= 0.0,
        Direction::Either => (a < 0.0 && b >= 0.0) || (a > 0.0 && b <= 0.0),
    }
}

/// State at `stop` from `start`.
pub fn propagate_until(
    p: &mut Propagator,
    start: &SpacecraftState,
    stop: Stop,
    count: usize,
    step_s: f64,
    max_duration_s: f64,
) -> Result<SpacecraftState, SequenceError> {
    let (quantity, value, direction) = match stop {
        Stop::Epoch(e) => {
            if e.seconds_since(start.epoch) < 0.0 {
                return Err(SequenceError::Invalid(format!(
                    "propagate to {e}: before the current epoch {}",
                    start.epoch
                )));
            }
            return Ok(p.propagate(start, e)?);
        }
        Stop::Duration(0.0) => return Ok(*start),
        Stop::Duration(d) => return Ok(p.propagate(start, start.epoch.advanced_by_seconds(d))?),
        Stop::Crossing {
            quantity,
            value,
            direction,
        } => (quantity, value, direction),
    };
    let angle = quantity.is_angle();
    let g = |s: &SpacecraftState| quantity.error(s, value);
    let mut found = 0;
    let mut left = *start;
    let mut g_left = g(&left);
    let mut elapsed = 0.0;
    while elapsed < max_duration_s {
        let h = step_s.min(max_duration_s - elapsed);
        let right = p.propagate(&left, left.epoch.advanced_by_seconds(h))?;
        let g_right = g(&right);
        if crosses(direction, g_left, g_right, angle) {
            let root = refine(p, &left, g_left, &right, g_right, &g)?;
            if root.epoch.seconds_since(start.epoch) > CROSSING_GUARD_S {
                found += 1;
                if found == count {
                    return Ok(root);
                }
            }
        }
        elapsed += h;
        (left, g_left) = (right, g_right);
    }
    Err(SequenceError::NotReached(format!(
        "{} {} {value} not reached {count}× within {max_duration_s} s",
        quantity.name(),
        match direction {
            Direction::Either => "crossing",
            Direction::Increasing => "rising through",
            Direction::Decreasing => "falling through",
        },
    )))
}

/// Illinois false position on `[a, b]`, each trial propagated from `a`; the result is
/// propagated to the secant root of the final bracket, so it varies smoothly with the inputs.
fn refine(
    p: &mut Propagator,
    a: &SpacecraftState,
    ga: f64,
    b: &SpacecraftState,
    gb: f64,
    g: &impl Fn(&SpacecraftState) -> f64,
) -> Result<SpacecraftState, SequenceError> {
    let span = b.epoch.seconds_since(a.epoch);
    // (t, g, Illinois-weighted g).
    let (mut lo, mut hi) = ((0.0, ga, ga), (span, gb, gb));
    let mut side = 0;
    for _ in 0..100 {
        if hi.0 - lo.0 <= CROSSING_TOLERANCE_S {
            break;
        }
        let t = if hi.2 != lo.2 {
            (lo.0 - lo.2 * (hi.0 - lo.0) / (hi.2 - lo.2)).clamp(lo.0, hi.0)
        } else {
            0.5 * (lo.0 + hi.0)
        };
        // Keep the bracket shrinking when false position stalls at an end.
        let t = if t - lo.0 < 0.1 * CROSSING_TOLERANCE_S || hi.0 - t < 0.1 * CROSSING_TOLERANCE_S {
            0.5 * (lo.0 + hi.0)
        } else {
            t
        };
        let s = p.propagate(a, a.epoch.advanced_by_seconds(t))?;
        let gt = g(&s);
        if gt == 0.0 {
            return Ok(s);
        }
        if (gt < 0.0) == (lo.1 < 0.0) {
            lo = (t, gt, gt);
            if side == -1 {
                hi.2 *= 0.5;
            }
            side = -1;
        } else {
            hi = (t, gt, gt);
            if side == 1 {
                lo.2 *= 0.5;
            }
            side = 1;
        }
    }
    let t = if hi.1 != lo.1 {
        (lo.0 - lo.1 * (hi.0 - lo.0) / (hi.1 - lo.1)).clamp(lo.0, hi.0)
    } else {
        hi.0
    };
    Ok(p.propagate(a, a.epoch.advanced_by_seconds(t))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::force::PointMass;
    use crate::propagate::{Integrator, Method};
    use crate::{Epoch, StateVector};

    fn two_body() -> Propagator {
        Propagator::new(Integrator::new(Method::Dop853).with_tolerance(1e-13, 1e-10)).with_force(
            PointMass {
                gm_km3_s2: EGM2008_GM_KM3_S2,
            },
        )
    }

    fn leo(radius_km: f64, inclination_deg: f64) -> SpacecraftState {
        let v = (EGM2008_GM_KM3_S2 / radius_km).sqrt();
        let i = inclination_deg.to_radians();
        SpacecraftState::new(
            Epoch::from_utc(2026, 1, 1, 0, 0, 0.0),
            StateVector::new(
                DVec3::new(radius_km, 0.0, 0.0),
                DVec3::new(0.0, v * i.cos(), v * i.sin()),
            ),
            1000.0,
        )
    }

    fn cmd(label: Option<&str>, action: Action) -> Command {
        Command {
            label: label.map(Into::into),
            action,
        }
    }

    fn until(stop: Stop) -> Action {
        Action::Propagate {
            stop,
            count: 1,
            step_s: 60.0,
            max_duration_s: 30.0 * 86_400.0,
        }
    }

    fn prograde(dv: f64) -> Action {
        Action::Maneuver {
            delta_v_km_s: DVec3::new(dv, 0.0, 0.0),
            frame: LocalFrame::Vnb,
            isp_s: Some(300.0),
        }
    }

    #[test]
    fn hohmann_burn_then_apoapsis_reaches_the_two_body_radius() {
        let (r1, r2) = (6678.137, 42_164.0);
        let mu = EGM2008_GM_KM3_S2;
        let dv = (mu / r1).sqrt() * ((2.0 * r2 / (r1 + r2)).sqrt() - 1.0);
        let out = run(
            &two_body(),
            leo(r1, 28.5),
            &[cmd(None, prograde(dv)), cmd(None, until(Stop::apoapsis()))],
        )
        .unwrap();
        let r = out.end.position_km.length();
        assert!((r - r2).abs() < 1e-3, "{r}");
        // Half the transfer period.
        let a = 0.5 * (r1 + r2);
        let half = std::f64::consts::PI * (a.powi(3) / mu).sqrt();
        let t = out.end.epoch.seconds_since(leo(r1, 0.0).epoch);
        assert!((t - half).abs() < 1e-3, "{t} vs {half}");
        assert!(out.end.mass_kg < 1000.0);
        assert_eq!(out.maneuvers.len(), 1);
        assert_eq!(out.segments.len(), 2);
    }

    #[test]
    fn nodes_and_count() {
        let s0 = leo(7000.0, 45.0);
        let mut p = two_body();
        // Starts at the ascending node: the guard skips it.
        let an = propagate_until(&mut p, &s0, Stop::ascending_node(), 1, 60.0, 86_400.0).unwrap();
        let period = 2.0 * std::f64::consts::PI * (7000.0f64.powi(3) / EGM2008_GM_KM3_S2).sqrt();
        assert!((an.epoch.seconds_since(s0.epoch) - period).abs() < 1e-3);
        let dn = propagate_until(&mut p, &s0, Stop::descending_node(), 2, 60.0, 86_400.0).unwrap();
        assert!((dn.epoch.seconds_since(s0.epoch) - 1.5 * period).abs() < 1e-3);
        assert!(dn.position_km.z.abs() < 1e-3);
        assert!(matches!(
            propagate_until(&mut p, &s0, Stop::ascending_node(), 1, 60.0, 600.0),
            Err(SequenceError::NotReached(_))
        ));
    }

    #[test]
    fn target_raises_apoapsis_to_geo() {
        let goal = 42_164.0;
        let out = run(
            &two_body(),
            leo(6678.137, 28.5),
            &[
                cmd(
                    Some("toi-target"),
                    Action::Target(Box::new(Target {
                        variables: vec![TargetVariable {
                            command: "toi".into(),
                            parameter: Parameter::DeltaV(0),
                            perturbation: 1e-5,
                            max_step: 0.5,
                        }],
                        goals: vec![Goal {
                            quantity: Quantity::RadiusKm,
                            value: goal,
                            tolerance: 0.01,
                        }],
                        body: vec![
                            cmd(Some("toi"), prograde(1.0)),
                            cmd(None, until(Stop::apoapsis())),
                        ],
                        max_iterations: 25,
                    })),
                ),
                cmd(None, until(Stop::Duration(600.0))),
            ],
        )
        .unwrap();
        let SegmentKind::Target(t) = &out.segments[0].kind else {
            panic!()
        };
        assert!(t.converged, "{t:?}");
        assert!((t.values[0] - 2.426).abs() < 0.01, "{}", t.values[0]);
        // Target, its body (maneuver, propagate), then the coast.
        assert_eq!(out.segments.len(), 4);
        assert_eq!(out.segments[1].depth, 1);
        assert!((out.segments[2].end.position_km.length() - goal).abs() < 0.01);
        assert_eq!(out.maneuvers.len(), 1);
    }

    #[test]
    fn finite_burn_duration_targets_apogee() {
        let goal = 7_500.0;
        let burn = Action::FiniteBurn {
            duration_s: 60.0,
            thrust_n: 400.0,
            isp_s: 300.0,
            direction: DVec3::X,
            frame: LocalFrame::Vnb.into(),
        };
        let out = run(
            &two_body(),
            leo(6678.137, 0.0),
            &[cmd(
                None,
                Action::Target(Box::new(Target {
                    variables: vec![TargetVariable {
                        command: "burn".into(),
                        parameter: Parameter::Duration,
                        perturbation: 1.0,
                        max_step: 300.0,
                    }],
                    goals: vec![Goal {
                        quantity: Quantity::ApogeeAltKm,
                        value: goal - 6378.137,
                        tolerance: 0.1,
                    }],
                    body: vec![cmd(Some("burn"), burn)],
                    max_iterations: 25,
                })),
            )],
        )
        .unwrap();
        let SegmentKind::Target(t) = &out.segments[0].kind else {
            panic!()
        };
        assert!(t.converged, "{t:?}");
        assert_eq!(out.finite_burns.len(), 1);
        assert!(out.end.mass_kg < 1000.0);
    }

    fn maneuver(dv: DVec3) -> Action {
        Action::Maneuver {
            delta_v_km_s: dv,
            frame: LocalFrame::Vnb,
            isp_s: Some(300.0),
        }
    }

    fn free(command: &str, component: usize) -> OptimizeVariable {
        OptimizeVariable {
            command: command.into(),
            parameter: Parameter::DeltaV(component),
            perturbation: 1e-6,
            max_step: 0.3,
            lower: f64::NEG_INFINITY,
            upper: f64::INFINITY,
        }
    }

    fn equal(quantity: Quantity, value: f64, tolerance: f64) -> Constraint {
        Constraint {
            quantity,
            relation: Relation::Equal,
            value,
            tolerance,
        }
    }

    #[test]
    fn optimize_splits_plane_change_between_perigee_and_apogee() {
        // LEO 28.5° → circular 42 164 km at 10°.
        let (r1, r2, mu) = (6678.137, 42_164.0, EGM2008_GM_KM3_S2);
        let inc = 10.0;
        let vp = (mu / r1).sqrt() * (2.0 * r2 / (r1 + r2)).sqrt();
        let va = vp * r1 / r2;
        let vc = (mu / r2).sqrt();
        let all_at_apogee = (vp - (mu / r1).sqrt())
            + (va * va + vc * vc - 2.0 * va * vc * 18.5_f64.to_radians().cos()).sqrt();

        let o = Optimize {
            variables: vec![
                free("toi", 0),
                free("toi", 1),
                free("goi", 0),
                free("goi", 1),
            ],
            objective: Objective::DeltaV,
            objective_tolerance: 1e-6,
            // |e| is not differentiable at 0: bound it instead of equating apsides.
            constraints: vec![
                equal(Quantity::SmaKm, r2, 0.01),
                Constraint {
                    quantity: Quantity::Eccentricity,
                    relation: Relation::AtMost,
                    value: 1e-4,
                    tolerance: 1e-6,
                },
                equal(Quantity::InclinationDeg, inc, 1e-4),
            ],
            body: vec![
                cmd(Some("toi"), maneuver(DVec3::new(2.4, 0.0, 0.0))),
                cmd(None, until(Stop::apoapsis())),
                cmd(Some("goi"), maneuver(DVec3::new(1.5, -0.4, 0.0))),
            ],
            max_iterations: 60,
        };
        let out = run(
            &two_body(),
            leo(r1, 28.5),
            &[cmd(Some("gto"), Action::Optimize(Box::new(o)))],
        )
        .unwrap();
        let SegmentKind::Optimize(t) = &out.segments[0].kind else {
            panic!()
        };
        assert!(t.converged, "{t:#?}");
        assert!(t.constraints.iter().all(|c| c.met));
        // Analytic optimum (1-D scan of the split): 4.04409 km/s, 1.59° at perigee; 13 m/s
        // below all-at-apogee.
        assert!((t.objective - 4.044_09).abs() < 1e-3, "{}", t.objective);
        assert!(all_at_apogee - t.objective > 0.012);
        assert!((t.objective - delta_v_km_s(&out.segments[1..])).abs() < 1e-9);
        let perigee_tilt = t.values[1]
            .atan2(t.values[0] + (mu / r1).sqrt())
            .to_degrees();
        assert!((perigee_tilt.abs() - 1.59).abs() < 0.05, "{perigee_tilt}");
        assert!((Quantity::InclinationDeg.of(&out.end) - inc).abs() < 1e-4);
    }

    #[test]
    fn optimize_respects_bounds_and_inequalities() {
        // Maximize apogee with a capped burn: bound wins over the inequality.
        let o = Optimize {
            variables: vec![OptimizeVariable {
                lower: 0.0,
                upper: 0.8,
                ..free("burn", 0)
            }],
            objective: Objective::Quantity {
                quantity: Quantity::ApogeeAltKm,
                maximize: true,
            },
            objective_tolerance: 1e-3,
            constraints: vec![Constraint {
                quantity: Quantity::ApogeeAltKm,
                relation: Relation::AtMost,
                value: 10_000.0,
                tolerance: 0.01,
            }],
            body: vec![cmd(Some("burn"), prograde(0.1))],
            max_iterations: 40,
        };
        let commands = [cmd(None, Action::Optimize(Box::new(o.clone())))];
        let out = run(&two_body(), leo(6678.137, 0.0), &commands).unwrap();
        let SegmentKind::Optimize(t) = &out.segments[0].kind else {
            panic!()
        };
        assert!(t.converged, "{t:#?}");
        // Apogee 10 000 km altitude needs ~1.3 km/s: the 0.8 bound is active.
        assert!((t.values[0] - 0.8).abs() < 1e-9, "{t:#?}");
        // Frozen: the body at the solution.
        let frozen = freeze(&commands, &out);
        assert_eq!(frozen, vec![cmd(Some("burn"), prograde(t.values[0]))]);
        let replay = run(&two_body(), leo(6678.137, 0.0), &frozen).unwrap();
        assert_eq!(replay.end, out.end);

        let mut tight = o;
        tight.constraints[0].value = 2_000.0;
        let out = run(
            &two_body(),
            leo(6678.137, 0.0),
            &[cmd(None, Action::Optimize(Box::new(tight)))],
        )
        .unwrap();
        let SegmentKind::Optimize(t) = &out.segments[0].kind else {
            panic!()
        };
        assert!(t.converged && t.constraints[0].met, "{t:#?}");
        assert!((t.objective - 2_000.0).abs() < 0.02, "{}", t.objective);
    }

    #[test]
    fn rejects_unknown_variable_and_backward_epoch() {
        let bad = Target {
            variables: vec![TargetVariable {
                command: "nope".into(),
                parameter: Parameter::DeltaV(0),
                perturbation: 1e-5,
                max_step: 0.1,
            }],
            goals: vec![Goal {
                quantity: Quantity::SmaKm,
                value: 7000.0,
                tolerance: 1.0,
            }],
            body: vec![],
            max_iterations: 5,
        };
        let s0 = leo(7000.0, 0.0);
        assert!(matches!(
            run(&two_body(), s0, &[cmd(None, Action::Target(Box::new(bad)))]),
            Err(SequenceError::Invalid(_))
        ));
        let back = until(Stop::Epoch(s0.epoch.advanced_by_seconds(-1.0)));
        assert!(run(&two_body(), s0, &[cmd(None, back)]).is_err());
    }

    #[test]
    fn quantity_names_round_trip() {
        for q in Quantity::ALL {
            assert_eq!(Quantity::from_name(q.name()), Some(q));
        }
    }
}
