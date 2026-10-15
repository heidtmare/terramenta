/**
 * Mission sequences: a spacecraft's `sequence`, ordered commands run by
 * `globe.runSequence` from its propagation's initial state. Each command has
 * an optional `label` and one action:
 *
 * - `propagate`: one of `durationS`, `epoch`, `to` (`periapsis`, `apoapsis`,
 *   `ascendingNode`, `descendingNode`), `quantity` + `value` (+ `direction`);
 *   crossings take `count`, `stepS`, `maxDurationS`.
 * - `maneuver`: `{deltaVKmS, frame?, ispS?}` at the current epoch.
 * - `finiteBurn`: `{durationS, thrustN, ispS, direction, frame?}` from it.
 * - `target`: `{variables: [{command, parameter}], goals: [{quantity, value,
 *   tolerance}], sequence, maxIterations?}`; variables name labelled commands in
 *   its `sequence` (`dv1`–`dv3` on a maneuver, `durationS` on a finite burn or a
 *   `durationS` propagate); goals hold at its end.
 *
 * The run resolves to the flown propagation (burn epochs, Δv, stop), which
 * every analysis takes unchanged.
 */

export const ACTIONS = ["propagate", "maneuver", "finiteBurn", "target"];
export const STOPS = ["durationS", "epoch", "to", "quantity"];
export const APSIDES_AND_NODES = ["periapsis", "apoapsis", "ascendingNode", "descendingNode"];

/** The command's action key, or `undefined` unless exactly one is set. */
export function actionOf(command) {
  const set = ACTIONS.filter((key) => command[key] !== undefined);
  return set.length === 1 ? set[0] : undefined;
}

/**
 * Structural errors beyond the schema (paths from `path`): one action per
 * command, one stop per propagate, unique labels, target variables naming a
 * labelled command of the right kind in their own body.
 */
export function sequenceErrors(sequence, path = "sequence") {
  const errors = [];
  const labels = new Set();
  const walk = (commands, at) =>
    commands.forEach((command, index) => {
      const here = `${at}[${index}]`;
      if (command.label !== undefined) {
        if (labels.has(command.label)) errors.push(`${here}.label: duplicate "${command.label}"`);
        labels.add(command.label);
      }
      const action = actionOf(command);
      if (!action) {
        errors.push(`${here}: exactly one of ${ACTIONS.join(", ")}`);
        return;
      }
      if (action === "propagate") {
        const stops = STOPS.filter((key) => command.propagate[key] !== undefined);
        if (stops.length !== 1) errors.push(`${here}.propagate: exactly one of ${STOPS.join(", ")}`);
        if (command.propagate.quantity !== undefined && command.propagate.value === undefined) {
          errors.push(`${here}.propagate: quantity needs value`);
        }
      }
      if (action === "target") {
        const body = command.target.sequence;
        command.target.variables.forEach((variable, v) => {
          const found = labelled(body, variable.command);
          const problem = !found
            ? `no command "${variable.command}" in this target's sequence`
            : !fits(found, variable.parameter)
              ? `"${variable.command}" has no ${variable.parameter}`
              : null;
          if (problem) errors.push(`${here}.target.variables[${v}]: ${problem}`);
        });
        walk(body, `${here}.target.sequence`);
      }
    });
  walk(sequence, path);
  return errors;
}

function labelled(commands, label) {
  for (const command of commands) {
    if (command.label === label) return command;
    const inner = command.target && labelled(command.target.sequence, label);
    if (inner) return inner;
  }
  return null;
}

function fits(command, parameter) {
  if (/^dv[123]$/.test(parameter)) return command.maneuver !== undefined;
  return command.finiteBurn !== undefined || command.propagate?.durationS !== undefined;
}

/** `{maneuvers, finiteBurns}` counts the sequence flies (each command runs once). */
export function burnCounts(sequence) {
  let maneuvers = 0;
  let finiteBurns = 0;
  const walk = (commands) => {
    for (const command of commands) {
      if (command.maneuver) maneuvers++;
      if (command.finiteBurn) finiteBurns++;
      if (command.target) walk(command.target.sequence);
    }
  };
  walk(sequence);
  return { maneuvers, finiteBurns };
}

/**
 * A project spacecraft's propagation as flown, for burn references before it
 * runs: with a sequence, placeholder burn lists of the sequence's counts.
 */
export function flownShape({ propagation, sequence }) {
  if (!sequence) return propagation;
  const { maneuvers, finiteBurns } = burnCounts(sequence);
  return { ...propagation, maneuvers: Array(maneuvers).fill({}), finiteBurns: Array(finiteBurns).fill({}) };
}

/** One-line summary of a command. */
export function describeCommand(command) {
  const label = command.label ? `${command.label}: ` : "";
  const p = command.propagate;
  if (p) {
    const nth = p.count > 1 ? ` #${p.count}` : "";
    if (p.durationS !== undefined) return `${label}propagate ${p.durationS} s`;
    if (p.epoch !== undefined) return `${label}propagate to ${p.epoch}`;
    if (p.to !== undefined) return `${label}propagate to ${p.to}${nth}`;
    return `${label}propagate to ${p.quantity} ${p.direction ?? "crossing"} ${p.value}${nth}`;
  }
  if (command.maneuver) {
    const dv = command.maneuver.deltaVKmS.map((value) => (value * 1000).toFixed(1)).join(", ");
    return `${label}maneuver [${dv}] m/s ${command.maneuver.frame ?? "vnb"}`;
  }
  if (command.finiteBurn) {
    const b = command.finiteBurn;
    return `${label}finite burn ${b.durationS} s at ${b.thrustN} N`;
  }
  if (command.target) {
    const t = command.target;
    return `${label}target ${t.variables.length} variable${t.variables.length > 1 ? "s" : ""} → ${t.goals.map((g) => g.quantity).join(", ")}`;
  }
  return `${label}?`;
}

const KIND = { propagate: "Propagate", maneuver: "Maneuver", finiteBurn: "Finite burn", target: "Target" };

/**
 * Report rows from a `runSequence` result: `[{name, depth, start, stop,
 * durationS, deltaVMS, massKg, altitudeKm, smaKm, eccentricity,
 * inclinationDeg, status}]`; `status` for targets.
 */
export function segmentRows(result) {
  return result.segments.map((segment) => ({
    name: segment.label ? `${KIND[segment.kind]} · ${segment.label}` : KIND[segment.kind],
    depth: segment.depth,
    start: segment.start,
    stop: segment.stop,
    durationS: segment.stop - segment.start,
    deltaVMS: segment.deltaVKmS === undefined ? null : segment.deltaVKmS * 1000,
    massKg: segment.massKg,
    altitudeKm: segment.elements.altitudeKm,
    smaKm: segment.elements.smaKm,
    eccentricity: segment.elements.eccentricity,
    inclinationDeg: segment.elements.inclinationDeg,
    status: segment.target
      ? `${segment.target.converged ? "converged" : "NOT converged"} in ${segment.target.iterations}`
      : null,
  }));
}
