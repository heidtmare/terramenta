# terramenta-solare

Solar-system mission planning: a hierarchical reference frame tree rooted at
the Solar System Barycentre, two-body propagation mechanics, a Lambert solver
for transfers between two bodies on two given dates, and a patched-conics
[`spacecraft::Spacecraft`](src/spacecraft.rs) that hands itself off between
whichever body's gravity currently dominates it, without its position ever
jumping to show for it.

![A heliocentric plot animating a spacecraft's flight from Earth to Mars: a yellow marker rides a Lambert-solved transfer arc from a launch flash at Earth to a landing flash at Mars, while Earth and Mars markers crawl along their own orbit rings at the pace the cruise actually took](docs/earth_to_mars.svg)

*Not drawn by hand: [`examples/earth_to_mars.rs`](examples/earth_to_mars.rs)
solves [`mission::plan_transfer`](src/mission.rs)'s underlying
[`lambert::solve`](src/lambert.rs) for a near-optimal Earth-Mars window,
spawns a [`spacecraft::Spacecraft`](src/spacecraft.rs) on that transfer orbit
at Earth's own position, and calls `Spacecraft::update` once an hour for the
whole cruise — the same call a real mission clock would make — until it
crosses into Mars' sphere of influence and is captured. The departure burn
itself is treated as instantaneous, the same simplification `plan_transfer`'s
own delta-v already makes; this is a heliocentric-only trip, silent about
escaping Earth's gravity well. The launch and landing flashes and the moving
Earth, Mars, and spacecraft markers are animated from that same sampled
flight data, on a loop.*

## What it computes

[`frame::FrameTree`](src/frame.rs) is the tree itself: the Solar System
Barycentre at the root, with [`sun::Sun`](src/sun.rs),
[`planets::EARTH`](src/planets.rs) and [`planets::MARS`](src/planets.rs)
hanging off it as direct children, each reporting its own position relative
to its parent rather than to one shared, distant origin — the fix for the
precision loss a single-origin representation would otherwise cost a
spacecraft trying to rendezvous with something a couple of hundred million
kilometres out.

[`orbit::OrbitalElements`](src/orbit.rs) is the two-body mechanics everything
in the tree propagates with: a state vector in, an orbit's classical elements
out, and a position and velocity at any other epoch back out of those.
[`lambert::solve`](src/lambert.rs) goes the other direction — two positions
and a transfer time in, the one two-body orbit connecting them out — and
[`mission::plan_transfer`](src/mission.rs) turns that into the delta-v an
actual departure and arrival date cost against the real planetary ephemeris,
alongside [`mission::hohmann_transfer`](src/mission.rs)'s always-optimal,
date-agnostic textbook baseline to judge it against.

[`spacecraft::Spacecraft`](src/spacecraft.rs) is where the tree earns its
keep. A spacecraft is described relative to whichever [`spacecraft::Primary`]
currently dominates it — Earth while still in Earth orbit, the Sun once
Earth's pull is no longer the better two-body approximation of what's shaping
its path — and `Spacecraft::update` checks both directions every tick: an
escape outward, to whatever the current primary itself orbits, and a capture
inward, into any body the primary names as a
[`spacecraft::Primary::capture_candidates`]. Crossing either boundary
reparents the spacecraft in [`frame::FrameTree`](src/frame.rs) with the state
recomputed relative to the new primary at that same instant, so its actual
position — its state relative to the SSB — never jumps to show for the
change in bookkeeping.

```rust
use terramenta_solare::spacecraft::{Primary, Spacecraft};
use terramenta_solare::{Epoch, StateVector, solar_system};
use glam::DVec3;

let mut tree = solar_system();
let (sun, earth) = (tree.find("Sun").unwrap(), tree.find("Earth").unwrap());

// Comfortably above local escape velocity at 7000 km from Earth's centre.
let departure = StateVector::new(DVec3::new(7000.0, 0.0, 0.0), DVec3::new(0.0, 11.5, 2.0));
let mut spacecraft =
    Spacecraft::spawn(&mut tree, "Escaper", Primary::earth(earth, sun), departure, Epoch::J2000);

let mut epoch = Epoch::J2000;
for _ in 0..(60 * 24) {
    epoch = epoch.advanced_by_seconds(3_600.0);
    if spacecraft.update(&mut tree, epoch) {
        break; // crossed Earth's sphere of influence — now heliocentric
    }
}
```

## Regenerating the image

```sh
cd terramenta-solare && cargo run --example earth_to_mars
```

Writes `docs/earth_to_mars.svg` in place, from the same near-optimal
Earth-Mars window this crate's own `mission` tests use, and prints the
departure/arrival dates, the heliocentric delta-v at each end, and the date
`Spacecraft::update` actually detects the Mars capture on. There is no
plotting library underneath — the example writes SVG elements by hand from
the sampled orbit rings and the followed trajectory, the same way
[`terramenta-charta`](../terramenta-charta/)'s `porkchop` example does.

## The web binding

Nothing yet — unlike [`terramenta-globe`](../terramenta-globe/) and
[`terramenta-charta`](../terramenta-charta/), this crate has no `wasm.rs` of
its own. `terramenta-charta` is its reference client, calling into this
crate's `mission` and `lambert` modules directly rather than through a
WebAssembly boundary of its own.
