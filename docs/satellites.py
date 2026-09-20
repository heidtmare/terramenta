#!/usr/bin/env python3
"""Generate docs/satellites.svg - an animated showcase of Terramenta's
ephemeris layer.  Everything drawn here is computed, not drawn by hand:
Keplerian propagation, ECI->ECEF rotation, orthographic projection and
analytic hidden-line removal against the globe.

The camera is Earth-fixed (ECEF), so the ground stands still: coastlines,
graticule and ground tracks are static geometry, while the orbit planes
precess, the satellites run and the terminator creeps around the world.

    python3 docs/satellites.py [land-110m.json] [docs/satellites.svg]

The coastline file is downloaded on first use.  The output is one SVG with no
script in it - every moving part is a SMIL animation, which is what lets it
run inside a README on GitHub.
"""
import json, math, os, random, sys

# ---------------------------------------------------------------- constants
MU = 398600.4418          # km^3/s^2
RE = 6378.137             # km
SIDEREAL = 86164.0905     # s

W, H = 1280, 680
CX, CY = 502.0, 348.0
RPX = 150.0               # Earth radius in px
EXP = 0.40                # radial compression exponent for display
LOOP = 45.0               # seconds per loop
SIM_DAYS = 2              # sidereal days the loop spans: every object here
                          # closes on one or two of them, so the loop is seamless
SIM = SIDEREAL * SIM_DAYS

CAM_LAT, CAM_LON = 28.0, 9.0
# a solstice sun: far enough from the camera axis that the terminator crosses
# the disc at every hour of the day rather than sliding off the limb
SUN_DEC, SUN_LON0 = -22.0, 62.0

INK = "#cfe0ff"
ACCENT = "#7fb2ff"
CYAN = "#8ce0ff"
AMBER = "#ffb757"
VIOLET = "#c3a6ff"
PINK = "#ff9ecf"
STEEL = "#7f9fd8"

def d2r(x): return x * math.pi / 180.0

# ------------------------------------------------------------------- linalg
def v_add(a, b): return (a[0]+b[0], a[1]+b[1], a[2]+b[2])
def v_mul(a, s): return (a[0]*s, a[1]*s, a[2]*s)
def v_dot(a, b): return a[0]*b[0] + a[1]*b[1] + a[2]*b[2]
def v_cross(a, b):
    return (a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0])
def v_norm(a):
    n = math.sqrt(v_dot(a, a))
    return (a[0]/n, a[1]/n, a[2]/n)
def rot_z(p, ang):
    c, s = math.cos(ang), math.sin(ang)
    return (p[0]*c - p[1]*s, p[0]*s + p[1]*c, p[2])

def sph(lat, lon, r=1.0):
    la, lo = d2r(lat), d2r(lon)
    return (r*math.cos(la)*math.cos(lo), r*math.cos(la)*math.sin(lo), r*math.sin(la))

# camera basis: CAMV points from the Earth's centre to the camera
CAMV = sph(CAM_LAT, CAM_LON)
RIGHT = v_norm(v_cross((0.0, 0.0, 1.0), CAMV))
UP = v_cross(CAMV, RIGHT)

def project(p):
    """3-vector in display units (Earth radius == RPX) -> (x, y, depth)."""
    return (CX + v_dot(p, RIGHT), CY - v_dot(p, UP), v_dot(p, CAMV))

def compress(r_km):
    """Map an orbital radius onto the screen.  Monotone, so ordering and
    relative speed survive; the radii themselves are compressed so that a
    geostationary ring and the ISS fit the same picture."""
    return RPX * (r_km / RE) ** EXP

def disp(p_km):
    """ECEF km -> display-space 3-vector."""
    r = math.sqrt(v_dot(p_km, p_km))
    return v_mul(p_km, compress(r) / r)

def occluded(p):
    """Is a display-space point hidden behind the globe?"""
    d = v_dot(p, CAMV)
    if d >= 0.0:
        return False
    perp2 = v_dot(p, p) - d*d
    return perp2 < RPX*RPX

def f(x, nd=1):
    s = f"{x:.{nd}f}"
    if s.endswith(".0"): s = s[:-2]
    return "0" if s == "-0" else s

# ------------------------------------------------------------------ orbits
def kepler(M, e):
    E = M if e < 0.8 else math.pi
    for _ in range(60):
        dE = (E - e*math.sin(E) - M) / (1 - e*math.cos(E))
        E -= dE
        if abs(dE) < 1e-12: break
    return E

def eci_at(el, t):
    """el = (a, e, inc, raan, argp, M0); t in seconds.  Returns ECI km."""
    a, e, inc, raan, argp, M0, n = el
    M = M0 + n*t
    E = kepler(M % (2*math.pi), e)
    nu = 2*math.atan2(math.sqrt(1+e)*math.sin(E/2), math.sqrt(1-e)*math.cos(E/2))
    r = a * (1 - e*math.cos(E))
    xp, yp = r*math.cos(nu), r*math.sin(nu)
    cw, sw = math.cos(argp), math.sin(argp)
    ci, si = math.cos(inc), math.sin(inc)
    co, so = math.cos(raan), math.sin(raan)
    x = xp*(cw*co - sw*so*ci) - yp*(sw*co + cw*so*ci)
    y = xp*(cw*so + sw*co*ci) - yp*(sw*so - cw*co*ci)
    z = xp*(sw*si) + yp*(cw*si)
    return (x, y, z)

def ecef_at(el, t):
    return rot_z(eci_at(el, t), -2*math.pi*t/SIDEREAL)

def elements(revs_per_day, e, inc, raan, argp, m0):
    """Periods are snapped to an integer number of revolutions per sidereal
    day so the loop closes seamlessly; the snap is under three percent for
    every orbit here, and the semi-major axis follows from the period."""
    n = 2*math.pi*revs_per_day/SIDEREAL
    a = (MU / (n*n)) ** (1.0/3.0)
    return (a, e, d2r(inc), d2r(raan), d2r(argp), d2r(m0), n)


# ------------------------------------------------------- surface geometry
def visible_runs(pts3, close=False):
    """Split a polyline of unit-sphere directions into the runs that face the
    camera, cutting each run at the limb.  Returns lists of (x, y)."""
    pts = list(pts3)
    if close and pts[0] != pts[-1]:
        pts.append(pts[0])
    runs, cur = [], []
    prev = None
    for p in pts:
        d = v_dot(p, CAMV)
        if prev is not None:
            pd = v_dot(prev, CAMV)
            if (d > 0) != (pd > 0):
                # cut at the limb: interpolate, then put the point back on
                # the sphere so the seam sits exactly on the silhouette
                tt = pd / (pd - d)
                q = v_norm(v_add(prev, v_mul(v_add(p, v_mul(prev, -1.0)), tt)))
                sx, sy, _ = project(v_mul(q, RPX))
                if d > 0:
                    cur = [(sx, sy)]
                else:
                    cur.append((sx, sy))
                    runs.append(cur); cur = []
        if d > 0:
            x, y, _ = project(v_mul(p, RPX))
            cur.append((x, y))
        prev = p
    if cur: runs.append(cur)
    return [r for r in runs if len(r) > 1]

def poly(pts, nd=1):
    return "M" + " ".join(f"{f(x,nd)},{f(y,nd)}" for x, y in pts)

def land_rings(path):
    """Natural Earth 110m land, decoded out of TopoJSON into rings of unit
    directions."""
    topo = json.load(open(path))
    sx, sy = topo["transform"]["scale"]
    tx, ty = topo["transform"]["translate"]
    arcs = []
    for arc in topo["arcs"]:
        x = y = 0
        out = []
        for dx, dy in arc:
            x += dx; y += dy
            out.append((x*sx + tx, y*sy + ty))
        arcs.append(out)
    def resolve(idx):
        return arcs[idx] if idx >= 0 else list(reversed(arcs[~idx]))
    rings = []
    for geom in topo["objects"]["land"]["geometries"]:
        polys = geom["arcs"] if geom["type"] == "MultiPolygon" else [geom["arcs"]]
        for pg in polys:
            for ring in pg:
                line = []
                for idx in ring:
                    part = resolve(idx)
                    line.extend(part if not line else part[1:])
                if len(line) > 3:
                    rings.append([sph(lat, lon) for lon, lat in line])
    return rings

def graticule(step=15, fine=3):
    lines = []
    for lon in range(-180, 180, step):
        lines.append([sph(lat, lon) for lat in range(-90, 91, fine)])
    for lat in range(-75, 76, step):
        lines.append([sph(lat, lon) for lon in range(-180, 181, fine)])
    out = []
    for l in lines:
        out.extend(visible_runs(l))
    return out

def ground_track(el, t0, t1, n):
    pts3 = []
    for i in range(n + 1):
        t = t0 + (t1 - t0) * i / n
        p = ecef_at(el, t)
        pts3.append(v_norm(p))
    return visible_runs(pts3)

# ------------------------------------------------------------ orbit rings
def plane_basis(el, t):
    """Orthonormal in-plane vectors of a circular orbit, in ECEF at time t."""
    _, _, inc, raan, argp, _, _ = el
    ci, si = math.cos(inc), math.sin(inc)
    co, so = math.cos(raan), math.sin(raan)
    u = (co, so, 0.0)
    w = (-so*ci, co*ci, si)
    g = -2*math.pi*t/SIDEREAL
    return rot_z(u, g), rot_z(w, g)

def ellipse_of(u, w, r, scale=1.0):
    """A circle in space projects to an ellipse.  Given the circle's two
    in-plane axes and its radius, this is that ellipse exactly: semi-axes,
    tilt, the screen-space images of the axes, the sense it is traced in, and
    the parameter at which it passes closest to the camera."""
    U = (r * scale * v_dot(u, RIGHT), -r * scale * v_dot(u, UP))
    V = (r * scale * v_dot(w, RIGHT), -r * scale * v_dot(w, UP))
    A, B, C, D = U[0], V[0], U[1], V[1]
    s1 = A*A + B*B + C*C + D*D
    s2 = math.hypot(A*A + B*B - C*C - D*D, 2*(A*C + B*D))
    rx = math.sqrt(max(s1 + s2, 0.0) / 2)
    ry = math.sqrt(max(s1 - s2, 0.0) / 2)
    ang = 0.5 * math.atan2(2*(A*C + B*D), A*A + B*B - C*C - D*D)
    return (rx, ry, math.degrees(ang), (U, V), A*D - B*C,
            math.atan2(v_dot(w, CAMV), v_dot(u, CAMV)))

def ring_params(el, t):
    """Analytic projection of a circular orbit: the ellipse it draws, plus
    the half of it that passes in front of the globe."""
    a = el[0]
    r = compress(a)
    u, w = plane_basis(el, t)
    # screen-space images of the two in-plane axes (SVG coords, y down)
    U = (r * v_dot(u, RIGHT), -r * v_dot(u, UP))
    V = (r * v_dot(w, RIGHT), -r * v_dot(w, UP))
    A, B, C, D = U[0], V[0], U[1], V[1]
    s1 = A*A + B*B + C*C + D*D
    s2 = math.hypot(A*A + B*B - C*C - D*D, 2*(A*C + B*D))
    rx = math.sqrt(max(s1 + s2, 0.0) / 2)
    ry = math.sqrt(max(s1 - s2, 0.0) / 2)
    ang = 0.5 * math.atan2(2*(A*C + B*D), A*A + B*B - C*C - D*D)
    det = A*D - B*C
    # the front half runs a half turn centred on the camera-facing point
    phi = math.atan2(v_dot(w, CAMV), v_dot(u, CAMV))
    return rx, ry, math.degrees(ang), (U, V), det, phi

def ring_point(UV, theta):
    U, V = UV
    return (CX + U[0]*math.cos(theta) + V[0]*math.sin(theta),
            CY + U[1]*math.cos(theta) + V[1]*math.sin(theta))

def continuous(prev, rx, ry, ang):
    """Keep (rx, ry, angle) on the branch nearest the previous keyframe so the
    interpolation between them is smooth rather than snapping by 90 degrees."""
    cands = []
    for k in (-2, -1, 0, 1, 2):
        cands.append((rx, ry, ang + 180*k))
        cands.append((ry, rx, ang + 90 + 180*k))
    if prev is None:
        return cands[0]
    prx, pry, pang = prev
    best = min(cands, key=lambda c: abs(c[2]-pang)*2 + abs(c[0]-prx) + abs(c[1]-pry))
    return best

# ------------------------------------------------------------- terminator
def sun_ecef(t):
    return rot_z(sph(SUN_DEC, SUN_LON0), -2*math.pi*t/SIDEREAL)

KF = 48                                   # keyframes for the slow animations

def times(n, days=1):
    """n+1 sample times spanning `days` sidereal days."""
    return [SIDEREAL * days * i / n for i in range(n + 1)]

def anim(attr, vals, days=1, extra=""):
    """Anything whose Earth-fixed motion repeats after `days` sidereal days is
    animated over that much of the loop and left to repeat."""
    dur = LOOP * days / SIM_DAYS
    return (f'<animate attributeName="{attr}" dur="{f(dur,2)}s" '
            f'repeatCount="indefinite" values="{";".join(vals)}"{extra}/>')

def _ang(x, y):
    """Angle about the centre of the disc, measured the way maths does it."""
    return math.atan2(CY - y, x - CX)

def night_frames(ts, n=32):
    """The night side of the visible hemisphere, frame by frame.

    Its boundary is half of the terminator great circle - which projects to an
    ellipse - closed along the limb round the anti-solar side.  The frames are
    generated together so the vertices stay in the same order as the sun goes
    round, which is what lets one path interpolate between them."""
    frames, qprev = [], None
    for t in ts:
        s = sun_ecef(t)
        q = v_norm(v_cross(s, CAMV))
        if qprev is not None and v_dot(q, qprev) < 0:
            q = v_mul(q, -1.0)
        qprev = q
        m = v_norm(v_add(CAMV, v_mul(s, -v_dot(CAMV, s))))   # nearest terminator point
        pts = []
        for i in range(n + 1):
            th = math.pi * i / n
            p = v_add(v_mul(q, math.cos(th)), v_mul(m, math.sin(th)))
            x, y, _ = project(v_mul(v_norm(p), RPX))
            pts.append((x, y))
        anti = v_norm(v_add(v_mul(s, -1.0), v_mul(CAMV, v_dot(s, CAMV))))
        ax, ay, _ = project(v_mul(anti, RPX))
        a0 = _ang(*pts[-1])
        a1 = _ang(*pts[0])
        aa = _ang(ax, ay)
        span = (a1 - a0) % (2*math.pi)
        if (aa - a0) % (2*math.pi) > span:
            span -= 2*math.pi
        pts.extend((CX + RPX*math.cos(a0 + span*k/n),
                    CY - RPX*math.sin(a0 + span*k/n)) for k in range(1, n))
        frames.append(pts)
    return frames

def _ring_orientation(ring3):
    """+1 if the ring runs counter-clockwise seen from outside the sphere, so
    that land is inside it and a hole is not."""
    c = v_norm([sum(p[k] for p in ring3) for k in range(3)])
    e1 = v_norm(v_cross(c, (0.0, 0.0, 1.0) if abs(c[2]) < 0.9 else (1.0, 0.0, 0.0)))
    e2 = v_cross(c, e1)
    flat = [(v_dot(p, e1), v_dot(p, e2)) for p in ring3]
    area = 0.0
    for k in range(len(flat)):
        x0, y0 = flat[k]
        x1, y1 = flat[(k + 1) % len(flat)]
        area += x0*y1 - x1*y0
    return 1.0 if area >= 0 else -1.0

def _limb_arc(a0, a1, sigma):
    """Points along the limb from a0 to a1, travelling in direction sigma."""
    span = ((a1 - a0) * sigma) % (2*math.pi)
    steps = max(1, int(span / 0.08))
    return [(CX + RPX*math.cos(a0 + sigma*span*k/steps),
             CY - RPX*math.sin(a0 + sigma*span*k/steps)) for k in range(1, steps)]

def visible_polys(ring3):
    """Clip a closed ring of unit directions against the visible hemisphere.

    The pieces that face the camera are kept as they are; where the ring passes
    behind the globe it is closed along the silhouette, travelling the way the
    ring itself runs, which is what keeps land inside the polygon and the ocean
    out of it however many times a continent crosses the limb."""
    pts = list(ring3)
    if pts[0] == pts[-1]: pts = pts[:-1]
    depths = [v_dot(p, CAMV) for p in pts]
    if max(depths) <= 0: return []
    if min(depths) > 0:
        return [[project(v_mul(p, RPX))[:2] for p in pts]]
    sigma = _ring_orientation(pts)
    start = next(i for i, d in enumerate(depths) if d <= 0)
    seq = pts[start:] + pts[:start]
    seq.append(seq[0])
    runs, cur = [], None
    prev = seq[0]
    for p in seq[1:]:
        dp, dn = v_dot(prev, CAMV), v_dot(p, CAMV)
        if (dn > 0) != (dp > 0):
            tt = dp / (dp - dn)
            q = v_norm(v_add(prev, v_mul(v_add(p, v_mul(prev, -1.0)), tt)))
            x, y, _ = project(v_mul(q, RPX))
            if dn > 0:
                cur = {"pts": [(x, y)], "in": _ang(x, y)}
            elif cur is not None:
                cur["pts"].append((x, y))
                cur["out"] = _ang(x, y)
                runs.append(cur); cur = None
        if dn > 0 and cur is not None:
            cur["pts"].append(project(v_mul(p, RPX))[:2])
        prev = p
    if not runs: return []
    polys, left = [], set(range(len(runs)))
    while left:
        first = min(left)
        chain, i = [], first
        while True:
            left.discard(i)
            chain.extend(runs[i]["pts"])
            nxt = min(range(len(runs)),
                      key=lambda j: ((runs[j]["in"] - runs[i]["out"]) * sigma)
                                    % (2*math.pi))
            chain.extend(_limb_arc(runs[i]["out"], runs[nxt]["in"], sigma))
            if nxt == first or nxt not in left:
                break
            i = nxt
        if len(chain) > 2: polys.append(chain)
    return polys

def rdp(pts, eps=0.35):
    """Ramer-Douglas-Peucker, so a coastline costs what it needs and no more."""
    if len(pts) < 3: return pts
    keep = [False]*len(pts); keep[0] = keep[-1] = True
    stack = [(0, len(pts)-1)]
    while stack:
        a, b = stack.pop()
        ax, ay = pts[a]; bx, by = pts[b]
        dx, dy = bx-ax, by-ay
        n2 = dx*dx + dy*dy
        worst, wi = -1.0, -1
        for i in range(a+1, b):
            px, py = pts[i]
            if n2 == 0:
                d = math.hypot(px-ax, py-ay)
            else:
                tt = max(0.0, min(1.0, ((px-ax)*dx + (py-ay)*dy)/n2))
                d = math.hypot(px-ax-tt*dx, py-ay-tt*dy)
            if d > worst: worst, wi = d, i
        if worst > eps:
            keep[wi] = True
            stack.append((a, wi)); stack.append((wi, b))
    return [p for p, k in zip(pts, keep) if k]

def track_of(sat, n, days):
    """Screen positions, visibility and sub-point of one satellite."""
    xs, ys, vis, sub = [], [], [], []
    for t in times(n, days):
        p = disp(ecef_at(sat["el"], t))
        x, y, _ = project(p)
        xs.append(f(x, 0)); ys.append(f(y, 0))
        vis.append("0" if occluded(p) else "1")
        g = v_norm(ecef_at(sat["el"], t))
        sx, sy, sd = project(v_mul(g, RPX))
        sub.append((sx, sy, sd > 0))
    return xs, ys, vis, sub

def shift(vals, lag):
    """The same closed cycle, delayed by `lag` samples - a trailing dot."""
    n = len(vals) - 1
    out = [vals[(i - lag) % n] for i in range(n)]
    out.append(out[0])
    return out

def dot(xs, ys, vis, r, colour, op, days, lag=0):
    sx, sy, sv = shift(xs, lag), shift(ys, lag), shift(vis, lag)
    return (f'<circle r="{f(r,2)}" fill="{colour}" opacity="{op}">'
            + anim("cx", sx, days) + anim("cy", sy, days)
            + anim("opacity", [v if v == "0" else str(op) for v in sv], days,
                   ' calcMode="discrete"')
            + '</circle>')

# ------------------------------------------------------------- the catalogue
# Periods are the real ones, rounded to a whole or half revolution per sidereal
# day so that every object returns to where it started when the loop does.
STATIONS = [
    {"name": "ISS (ZARYA)", "el": elements(15.5, 0.0004, 51.64, 47, 60, 0),
     "days": 2, "n": 496},
    {"name": "CSS (TIANHE)", "el": elements(15.5, 0.0006, 41.47, 212, 30, 140),
     "days": 2, "n": 496},
]
GPS = [{"name": f"GPS BIIF-{i}", "el": elements(2, 0.008, 55.0, raan, 40, m0),
        "days": 1, "n": 96}
       for i, (raan, m0) in enumerate(
           [(15, 0), (15, 155), (135, 60), (135, 230), (255, 100), (255, 285)], 1)]
MOLNIYA = [
    {"name": "MOLNIYA 3-50", "el": elements(2, 0.74, 63.4, 40, 270, 10),
     "days": 1, "n": 144},
    {"name": "MOLNIYA 1-91", "el": elements(2, 0.74, 63.4, 165, 270, 190),
     "days": 1, "n": 144},
]
STARLINK = [{"name": "STARLINK", "el": elements(15, 0.0002, 53.0, (i*29) % 360,
                                                0, (i*67) % 360),
             "days": 1, "n": 210}
            for i in range(20)]
GEO = [{"name": "GEO", "el": elements(1, 0.0, 0.5, (i*23) % 360, 0, (i*11) % 360),
        "days": 1, "n": 4}
       for i in range(18)]

CITIES = [
    (40.7, -74.0), (34.0, -118.2), (41.9, -87.6), (29.8, -95.4), (19.4, -99.1),
    (45.5, -73.6), (43.7, -79.4), (-23.5, -46.6), (-34.6, -58.4), (4.7, -74.1),
    (-12.0, -77.0), (-33.4, -70.7), (51.5, -0.1), (48.9, 2.4), (40.4, -3.7),
    (41.9, 12.5), (52.5, 13.4), (55.8, 37.6), (59.3, 18.1), (50.1, 14.4),
    (41.0, 29.0), (30.0, 31.2), (6.5, 3.4), (-26.2, 28.0), (-33.9, 18.4),
    (-1.3, 36.8), (24.7, 46.7), (25.3, 55.3), (35.7, 51.4), (28.6, 77.2),
    (19.1, 72.9), (13.1, 80.3), (23.8, 90.4), (13.8, 100.5), (1.35, 103.8),
    (-6.2, 106.8), (14.6, 121.0), (22.3, 114.2), (31.2, 121.5), (39.9, 116.4),
    (37.6, 127.0), (35.7, 139.7), (34.7, 135.5), (25.0, 121.5), (-33.9, 151.2),
    (-37.8, 145.0), (-41.3, 174.8), (61.2, -149.9), (64.1, -21.9), (37.8, -122.4),
    (55.0, 82.9), (43.1, 131.9), (21.0, 105.8), (12.0, 8.5), (-15.8, -47.9),
    (36.8, 10.2), (33.6, -7.6), (5.6, -0.2), (-4.3, 15.3), (-18.9, 47.5),
    (60.2, 24.9), (53.3, -6.3), (38.7, -9.1), (45.4, 9.2), (47.5, 19.0),
    (44.4, 26.1), (54.7, 25.3), (56.9, 24.1), (35.0, 135.8), (-8.7, 115.2),
]

# ------------------------------------------------------------------ emission
NKF = 72                                  # terminator keyframes (one day)

def defs():
    night = [poly(p, 0) + "Z" for p in night_frames(times(NKF))]
    return f'''<defs>
<radialGradient id="space" cx="50%" cy="44%" r="74%">
  <stop offset="0" stop-color="#0c1628"/><stop offset="0.55" stop-color="#060b16"/>
  <stop offset="1" stop-color="#02040a"/>
</radialGradient>
<radialGradient id="ocean" cx="36%" cy="30%" r="80%">
  <stop offset="0" stop-color="#1e528f"/><stop offset="0.6" stop-color="#123763"/>
  <stop offset="1" stop-color="#071930"/>
</radialGradient>
<radialGradient id="limb" cx="50%" cy="50%" r="50%">
  <stop offset="0.78" stop-color="#3f7fd8" stop-opacity="0"/>
  <stop offset="0.845" stop-color="#4f95e8" stop-opacity="0.26"/>
  <stop offset="0.857" stop-color="#8fd0ff" stop-opacity="0.50"/>
  <stop offset="0.93" stop-color="#5fa8f0" stop-opacity="0.10"/>
  <stop offset="1" stop-color="#4f95e8" stop-opacity="0"/>
</radialGradient>
<radialGradient id="glint" cx="50%" cy="50%" r="50%">
  <stop offset="0" stop-color="#fff6d8" stop-opacity="0.75"/>
  <stop offset="0.45" stop-color="#ffe6a0" stop-opacity="0.18"/>
  <stop offset="1" stop-color="#ffd27f" stop-opacity="0"/>
</radialGradient>
<filter id="soft" x="-30%" y="-30%" width="160%" height="160%">
  <feGaussianBlur stdDeviation="6"/>
</filter>
<clipPath id="disc"><circle cx="{f(CX)}" cy="{f(CY)}" r="{f(RPX)}"/></clipPath>
<mask id="nightmask">
  <rect width="{W}" height="{H}" fill="#000"/>
  <path fill="#fff" filter="url(#soft)" d="{night[0]}">
    {anim("d", night)}
  </path>
</mask>
</defs>'''

def starfield():
    rnd = random.Random(20260920)
    out = ['<g id="stars">']
    for _ in range(340):
        x, y = rnd.uniform(0, W), rnd.uniform(0, H)
        if math.hypot(x - CX, y - CY) < RPX + 5:
            continue
        r = rnd.choice([0.5, 0.6, 0.7, 0.8, 0.9, 1.1, 1.4])
        o = rnd.uniform(0.18, 0.85)
        tw = ""
        if rnd.random() < 0.16:
            d = rnd.uniform(2.4, 6.5)
            tw = (f'<animate attributeName="opacity" dur="{d:.1f}s" '
                  f'repeatCount="indefinite" values="{o:.2f};{o*0.25:.2f};{o:.2f}"/>')
        colour = "#dfe9ff" if rnd.random() < 0.8 else rnd.choice(["#ffd9c0", "#c8dcff"])
        out.append(f'<circle cx="{f(x,0)}" cy="{f(y,0)}" r="{r}" fill="{colour}" '
                   f'opacity="{o:.2f}">{tw}</circle>')
    out.append("</g>")
    return "".join(out)

def rings(cat, colour, width, op, front_op, kf=KF):
    """Every circular orbit is a circle in space, so it projects to an ellipse:
    the whole ring goes behind the globe, and the half that passes in front is
    drawn over it.  Both are exact - only the parameters are animated, once per
    sidereal day, because that is how long the frame takes to turn under them."""
    back, front = [], []
    for sat in cat:
        rxs, rys, angs, ds = [], [], [], []
        prev = None
        for t in times(kf):
            rx, ry, ang, UV, det, phi = ring_params(sat["el"], t)
            rx, ry, ang = continuous(prev, rx, ry, ang)
            prev = (rx, ry, ang)
            rxs.append(f(rx)); rys.append(f(ry)); angs.append(f(ang))
            p1 = ring_point(UV, phi - math.pi/2)
            p2 = ring_point(UV, phi + math.pi/2)
            ds.append(f"M{f(p1[0],0)},{f(p1[1],0)}A{f(rx)},{f(ry)},{f(ang)},0,"
                      f"{1 if det > 0 else 0},{f(p2[0],0)},{f(p2[1],0)}")
        static = (max(map(float, rxs)) - min(map(float, rxs)) < 0.4 and
                  max(map(float, rys)) - min(map(float, rys)) < 0.4)
        if static:
            back.append(f'<g transform="translate({f(CX)},{f(CY)}) rotate({angs[0]})">'
                        f'<ellipse rx="{rxs[0]}" ry="{rys[0]}" fill="none" '
                        f'stroke="{colour}" stroke-width="{width}" opacity="{op}"/></g>')
            front.append(f'<path d="{ds[0]}" fill="none" stroke="{colour}" '
                         f'stroke-width="{width}" opacity="{front_op}"/>')
        else:
            back.append(
                f'<g transform="translate({f(CX)},{f(CY)})"><g>'
                f'<animateTransform attributeName="transform" type="rotate" '
                f'dur="{f(LOOP/SIM_DAYS,2)}s" repeatCount="indefinite" '
                f'values="{";".join(angs)}"/>'
                f'<ellipse rx="{rxs[0]}" ry="{rys[0]}" fill="none" stroke="{colour}" '
                f'stroke-width="{width}" opacity="{op}">'
                + anim("rx", rxs) + anim("ry", rys) + '</ellipse></g></g>')
            front.append(f'<path fill="none" stroke="{colour}" stroke-width="{width}" '
                         f'opacity="{front_op}" d="{ds[0]}">' + anim("d", ds) + '</path>')
    return "".join(back), "".join(front)

def ellipse_orbit(sat, colour, width, op, kf=40, n=48):
    """An eccentric orbit, sampled: the radial compression turns its ellipse
    into an egg, so it cannot be drawn as one."""
    frames = []
    a, e = sat["el"][0], sat["el"][1]
    for t in times(kf):
        pts = []
        for k in range(n + 1):
            E = 2*math.pi*k/n
            el = (a, e, sat["el"][2], sat["el"][3], sat["el"][4], E - e*math.sin(E), 0.0)
            x, y, _ = project(disp(rot_z(eci_at(el, 0.0), -2*math.pi*t/SIDEREAL)))
            pts.append((x, y))
        frames.append(poly(pts, 0))
    return (f'<path fill="none" stroke="{colour}" stroke-width="{width}" '
            f'opacity="{op}" d="{frames[0]}">' + anim("d", frames) + '</path>')

def globe(landpath):
    land = []
    for r in land_rings(landpath):
        for pg in visible_polys(r):
            land.append(poly(rdp(pg, 0.35)) + "Z")
    grat = [poly(rdp(g, 0.25)) for g in graticule()]
    # ground tracks: fixed to the Earth, so in this frame they stand still
    tracks = []
    for sat, wid, op in [(GPS[0], 1.5, 0.92), (GPS[3], 1.5, 0.62),
                         (MOLNIYA[0], 1.3, 0.55)]:
        revs = sat["el"][6] * SIDEREAL / (2*math.pi)
        for run in ground_track(sat["el"], 0, SIDEREAL, int(220 * revs)):
            tracks.append(f'<path d="{poly(rdp(run, 0.3))}" fill="none" '
                          f'stroke="{AMBER}" stroke-width="{wid}" opacity="{op}" '
                          f'stroke-linecap="round" stroke-linejoin="round"/>')
    lights = []
    for lat, lon in CITIES:
        n = sph(lat, lon)
        x, y, d = project(v_mul(n, RPX))
        if d <= 0.03: continue
        vals = []
        for t in times(KF):
            dark = max(0.0, min(1.0, -v_dot(n, sun_ecef(t)) * 4.0))
            vals.append(f"{dark * 0.9 * min(1.0, d/RPX*3.0):.2f}")
        lights.append(f'<circle cx="{f(x)}" cy="{f(y)}" r="1.7" fill="#ffe0b0" '
                      f'opacity="0">{anim("opacity", vals)}</circle>')
    gx, gy, go = [], [], []
    for t in times(KF):
        x, y, d = project(v_mul(sun_ecef(t), RPX))
        gx.append(f(x, 0)); gy.append(f(y, 0))
        go.append(f"{max(0.0, min(1.0, (d/RPX - 0.12) * 1.5)):.2f}")
    glint = (f'<circle cx="{gx[0]}" cy="{gy[0]}" r="{f(RPX*0.42)}" fill="url(#glint)" '
             f'opacity="0">' + anim("cx", gx) + anim("cy", gy) + anim("opacity", go)
             + '</circle>')
    return f'''<g id="globe">
<circle cx="{f(CX)}" cy="{f(CY)}" r="{f(RPX*1.17)}" fill="url(#limb)"/>
<circle cx="{f(CX)}" cy="{f(CY)}" r="{f(RPX)}" fill="url(#ocean)"/>
<g clip-path="url(#disc)">
  <path d="{"".join(land)}" fill="#245741" fill-opacity="0.95" stroke="#86e0ba"
        stroke-opacity="0.30" stroke-width="0.45"/>
  <path d="{"".join(grat)}" fill="none" stroke="{AMBER}" stroke-opacity="0.16"
        stroke-width="0.6"/>
  {"".join(tracks)}
  {glint}
  <circle cx="{f(CX)}" cy="{f(CY)}" r="{f(RPX)}" fill="#000814" opacity="0.80"
          mask="url(#nightmask)"/>
  {"".join(lights)}
</g>
<circle cx="{f(CX)}" cy="{f(CY)}" r="{f(RPX)}" fill="none" stroke="#9fd0ff"
        stroke-opacity="0.42" stroke-width="0.9"/>
</g>'''

def equator():
    out = []
    for run in visible_runs([sph(0, lon) for lon in range(-180, 181, 3)]):
        out.append(f'<path d="{poly(rdp(run, 0.25))}" fill="none" stroke="{AMBER}" '
                   f'stroke-opacity="0.40" stroke-width="1"/>')
    return "".join(out)

def satellites():
    out = []
    def group(cat, colour, r, trail, op=1.0, label=None, lag=2):
        for sat in cat:
            xs, ys, vis, _ = track_of(sat, sat["n"], sat["days"])
            for j in range(trail, 0, -1):
                out.append(dot(xs, ys, vis, r*0.6, colour,
                               round(0.34 - 0.045*j, 3), sat["days"], lag=j*lag))
            out.append(dot(xs, ys, vis, r, colour, op, sat["days"]))
            if label:
                lx = [f(float(v) + 12, 0) for v in xs]
                ly = [f(float(v) - 10, 0) for v in ys]
                out.append(
                    f'<text font-size="12" fill="{colour}" opacity="0" '
                    f'letter-spacing="0.9" x="{lx[0]}" y="{ly[0]}">{label}'
                    + anim("x", lx, sat["days"]) + anim("y", ly, sat["days"])
                    + anim("opacity", [v if v == "0" else "0.9" for v in vis],
                           sat["days"], ' calcMode="discrete"') + '</text>')
    group(STARLINK, STEEL, 1.7, 0, 0.82)
    group(GEO, "#ffd28a", 2.0, 0, 0.9)
    group(GPS, VIOLET, 2.5, 4, 0.95, lag=1)
    group(MOLNIYA, PINK, 2.5, 4, 0.95, lag=1)
    group(STATIONS[1:], CYAN, 2.9, 3, 1.0, lag=3)
    group(STATIONS[:1], CYAN, 3.3, 4, 1.0, label="ISS (ZARYA)", lag=3)
    # the tether: the station, and the point on the ground under it
    sat = STATIONS[0]
    xs, ys, vis, sub = track_of(sat, sat["n"], sat["days"])
    sx = [f(p[0], 0) for p in sub]; sy = [f(p[1], 0) for p in sub]
    svis = ["1" if p[2] else "0" for p in sub]
    both = ["0.45" if a == "1" and b == "1" else "0" for a, b in zip(vis, svis)]
    out.append(f'<line stroke="{CYAN}" stroke-width="0.8" stroke-dasharray="2 3" '
               f'stroke-opacity="0" x1="{xs[0]}" y1="{ys[0]}" x2="{sx[0]}" '
               f'y2="{sy[0]}">'
               + anim("x1", xs, sat["days"]) + anim("y1", ys, sat["days"])
               + anim("x2", sx, sat["days"]) + anim("y2", sy, sat["days"])
               + anim("stroke-opacity", both, sat["days"], ' calcMode="discrete"')
               + '</line>')
    out.append(f'<circle r="2.4" fill="none" stroke="{AMBER}" stroke-width="1.2" '
               f'opacity="0" cx="{sx[0]}" cy="{sy[0]}">'
               + anim("cx", sx, sat["days"]) + anim("cy", sy, sat["days"])
               + anim("opacity", svis, sat["days"], ' calcMode="discrete"')
               + '</circle>')
    return "".join(out)

# --------------------------------------------------------------- the inset
# The same propagation, drawn in the other frame.  Against the stars the orbit
# is a closed ellipse that does not move; it is the Earth that turns under it.
IX, IY, IR = 1097.0, 520.0, 58.0

def _ipt(p, r=1.0):
    """Project a direction into the inset, `r` in Earth radii."""
    return (IX + IR*r*v_dot(p, RIGHT), IY - IR*r*v_dot(p, UP), v_dot(p, CAMV))

def _iell(u, w, r, colour, width, op, frames=None, days=1):
    """An inset ellipse, optionally with its parameters animated."""
    if frames is None:
        rx, ry, ang, *_ = ellipse_of(u, w, r, IR)
        return (f'<g transform="translate({f(IX)},{f(IY)}) rotate({f(ang)})">'
                f'<ellipse rx="{f(rx)}" ry="{f(ry)}" fill="none" stroke="{colour}" '
                f'stroke-width="{width}" opacity="{op}"/></g>')
    rxs, rys, angs, prev = [], [], [], None
    for uu, ww in frames:
        rx, ry, ang, *_ = ellipse_of(uu, ww, r, IR)
        rx, ry, ang = continuous(prev, rx, ry, ang)
        prev = (rx, ry, ang)
        rxs.append(f(rx)); rys.append(f(ry)); angs.append(f(ang))
    return (f'<g transform="translate({f(IX)},{f(IY)})"><g>'
            f'<animateTransform attributeName="transform" type="rotate" '
            f'dur="{f(LOOP*days/SIM_DAYS,2)}s" repeatCount="indefinite" '
            f'values="{";".join(angs)}"/>'
            f'<ellipse rx="{rxs[0]}" ry="{rys[0]}" fill="none" stroke="{colour}" '
            f'stroke-width="{width}" opacity="{op}">'
            + anim("rx", rxs, days) + anim("ry", rys, days) + '</ellipse></g></g>')

def inset():
    px, py, pw, ph = 950.0, 404.0, 294.0, 232.0
    out = [f'<rect x="{f(px)}" y="{f(py)}" width="{f(pw)}" height="{f(ph)}" rx="4" '
           f'fill="#060a12" fill-opacity="0.74" stroke="{INK}" stroke-opacity="0.16"/>',
           f'<text x="{f(px+16)}" y="{f(py+24)}" font-size="11" letter-spacing="2.6" '
           f'fill="{ACCENT}" opacity="0.85">ECI &#183; THE SAME ORBIT</text>']
    rnd = random.Random(7)
    for _ in range(26):
        x = rnd.uniform(px + 8, px + pw - 8)
        y = rnd.uniform(py + 34, py + ph - 34)
        if math.hypot(x - IX, y - IY) < IR + 4: continue
        out.append(f'<circle cx="{f(x,0)}" cy="{f(y,0)}" r="{rnd.choice([0.5,0.7,0.9])}" '
                   f'fill="#dfe9ff" opacity="{rnd.uniform(0.2,0.7):.2f}"/>')
    # the Earth, wireframe, turning: meridians are great circles through the
    # poles, so each is an ellipse whose parameters follow the sidereal angle
    kf = 40
    for k in range(6):
        frames = []
        for t in times(kf):
            lam = math.pi*k/6 + 2*math.pi*t/SIDEREAL
            frames.append(((math.cos(lam), math.sin(lam), 0.0), (0.0, 0.0, 1.0)))
        out.append(_iell(None, None, 1.0, AMBER, 0.6, 0.30, frames=frames))
    out.append(f'<circle cx="{f(IX)}" cy="{f(IY)}" r="{f(IR)}" fill="#0a1d33" '
               f'fill-opacity="0.55"/>')
    for lat in (-60, -30, 0, 30, 60):
        c = math.cos(d2r(lat))
        z = math.sin(d2r(lat))
        rx, ry, ang, *_ = ellipse_of((1.0, 0.0, 0.0), (0.0, 1.0, 0.0), c, IR)
        cx = IX + IR*z*v_dot((0.0, 0.0, 1.0), RIGHT)
        cy = IY - IR*z*v_dot((0.0, 0.0, 1.0), UP)
        out.append(f'<g transform="translate({f(cx)},{f(cy)}) rotate({f(ang)})">'
                   f'<ellipse rx="{f(rx)}" ry="{f(ry)}" fill="none" stroke="{AMBER}" '
                   f'stroke-width="{0.9 if lat == 0 else 0.6}" '
                   f'opacity="{0.42 if lat == 0 else 0.22}"/></g>')
    out.append(f'<circle cx="{f(IX)}" cy="{f(IY)}" r="{f(IR)}" fill="none" '
               f'stroke="#9fd0ff" stroke-opacity="0.38" stroke-width="0.8"/>')
    # the station's orbit: in this frame it is one closed ellipse, fixed
    sat = STATIONS[0]
    el = sat["el"]
    period = 2*math.pi/el[6]
    ring = []
    for i in range(97):
        p = eci_at(el, period*i/96)
        r = math.sqrt(v_dot(p, p))
        x, y, _ = _ipt(v_mul(p, 1.0/r), r/RE)
        ring.append((x, y))
    out.append(f'<path d="{poly(ring)}Z" fill="none" stroke="{CYAN}" '
               f'stroke-width="1.3" opacity="0.75"/>')
    xs, ys, vis = [], [], []
    for i in range(49):
        p = eci_at(el, period*i/48)
        r = math.sqrt(v_dot(p, p))
        x, y, d = _ipt(v_mul(p, 1.0/r), r/RE)
        xs.append(f(x)); ys.append(f(y))
        perp2 = (r/RE*IR)**2 - (d*r/RE*IR)**2
        vis.append("0" if d < 0 and perp2 < IR*IR else "1")
    days = period/SIDEREAL
    for lag in (3, 2, 1):
        out.append(dot(xs, ys, vis, 1.4, CYAN, round(0.34 - 0.08*lag, 2), days))
    out.append(dot(xs, ys, vis, 2.6, CYAN, 1.0, days))
    out.append(f'<text x="{f(px+16)}" y="{f(py+ph-32)}" font-size="11" '
               f'fill="{INK}" opacity="0.5" letter-spacing="0.9">'
               f'ONE PROPAGATION, DRAWN TWICE:</text>')
    out.append(f'<text x="{f(px+16)}" y="{f(py+ph-15)}" font-size="11" '
               f'fill="{INK}" opacity="0.5" letter-spacing="0.9">'
               f'CLOSED HERE, A CORKSCREW OVER THERE</text>')
    return "".join(out)

# ----------------------------------------------------------------------- HUD
TICK = 36

def ticker(x, y, values, days, size=13, fill=INK, anchor="start"):
    """A readout that changes: one text per keyframe, shown in turn."""
    out, n = [], len(values)
    for i, v in enumerate(values):
        vals = ["0"] * n
        vals[i] = "1"
        out.append(f'<text x="{f(x)}" y="{f(y)}" font-size="{size}" fill="{fill}" '
                   f'text-anchor="{anchor}" opacity="0">{v}'
                   + anim("opacity", vals + ["0"], days, ' calcMode="discrete"')
                   + '</text>')
    return "".join(out)

def iss_readout():
    lat, alt, vel, clock = [], [], [], []
    a = STATIONS[0]["el"][0]
    for i in range(TICK):
        t = SIM * i / TICK
        p = ecef_at(STATIONS[0]["el"], t)
        r = math.sqrt(v_dot(p, p))
        la = math.degrees(math.asin(p[2]/r))
        lo = math.degrees(math.atan2(p[1], p[0]))
        lat.append(f"{abs(la):.2f}&#176;{'N' if la >= 0 else 'S'} "
                   f"{abs(lo):.2f}&#176;{'E' if lo >= 0 else 'W'}")
        alt.append(f"{r - RE:.1f} km")
        vel.append(f"{math.sqrt(MU*(2/r - 1/a)):.3f} km/s")
        secs = int(t) % 86400
        clock.append(f"{secs//3600:02d}:{secs%3600//60:02d}:{secs%60:02d}Z")
    return lat, alt, vel, clock

def hud():
    lat, alt, vel, clock = iss_readout()
    px, py, pw = 950.0, 92.0, 294.0
    out = ['<g id="hud">',
           f'<text x="44" y="62" font-size="34" letter-spacing="8.5" fill="{INK}">'
           f'TERRAMENTA</text>',
           f'<text x="46" y="85" font-size="12" letter-spacing="2.4" fill="{INK}" '
           f'opacity="0.5">A NAVIGABLE 3D GLOBE &#183; RUST &#183; BEVY &#183; '
           f'WEBGPU &#183; WEBASSEMBLY</text>',
           f'<rect x="{f(px)}" y="{f(py)}" width="{f(pw)}" height="296" rx="4" '
           f'fill="#060a12" fill-opacity="0.74" stroke="{INK}" stroke-opacity="0.16"/>',
           f'<text x="{f(px+16)}" y="{f(py+24)}" font-size="11" letter-spacing="2.6" '
           f'fill="{ACCENT}" opacity="0.85">EPHEMERIS LAYER</text>',
           f'<line x1="{f(px+16)}" y1="{f(py+34)}" x2="{f(px+pw-16)}" y2="{f(py+34)}" '
           f'stroke="{INK}" stroke-opacity="0.14"/>']
    y = py + 56
    for k, v in [("FRAME", "ECEF &#183; earth-fixed"), ("CLOCK", None),
                 ("PROPAGATOR", "SGP4 &#183; OMM"), ("CATALOGUE", "celestrak GP"),
                 ("HELD", "5,214 objects"), ("DRAWN", "1,024 markers")]:
        out.append(f'<text x="{f(px+16)}" y="{f(y)}" font-size="11" '
                   f'letter-spacing="1.6" fill="{INK}" opacity="0.45">{k}</text>')
        if v is None:
            out.append(ticker(px + pw - 16, y, clock, SIM_DAYS, 13, INK, "end"))
        else:
            out.append(f'<text x="{f(px+pw-16)}" y="{f(y)}" font-size="13" '
                       f'fill="{INK}" text-anchor="end">{v}</text>')
        y += 22
    y += 12
    out.append(f'<line x1="{f(px+16)}" y1="{f(y-20)}" x2="{f(px+pw-16)}" y2="{f(y-20)}" '
               f'stroke="{INK}" stroke-opacity="0.14"/>')
    out.append(f'<text x="{f(px+16)}" y="{f(y)}" font-size="11" letter-spacing="2.6" '
               f'fill="{ACCENT}" opacity="0.85">PICKED</text>')
    y += 24
    out.append(f'<text x="{f(px+16)}" y="{f(y)}" font-size="15" fill="{CYAN}" '
               f'letter-spacing="0.6">ISS (ZARYA)</text>')
    y += 17
    out.append(f'<text x="{f(px+16)}" y="{f(y)}" font-size="10.5" fill="{INK}" '
               f'opacity="0.45" letter-spacing="1.1">NORAD 25544 &#183; '
               f'EPOCH 0.4 h OLD</text>')
    y += 25
    for k, vals in (("SUB-POINT", lat), ("ALTITUDE", alt), ("SPEED", vel)):
        out.append(f'<text x="{f(px+16)}" y="{f(y)}" font-size="11" '
                   f'letter-spacing="1.6" fill="{INK}" opacity="0.45">{k}</text>')
        out.append(ticker(px + pw - 16, y, vals, SIM_DAYS, 13, CYAN, "end"))
        y += 20
    legend = [(CYAN, "crewed stations &#183; 420 km"),
              (STEEL, "starlink &#183; 550 km shell"),
              (VIOLET, "gps &#183; 12 h medium orbit"),
              (PINK, "molniya &#183; e 0.74 ellipse"),
              ("#ffd28a", "geostationary &#183; 35,786 km"),
              (AMBER, "ground track &#183; earth-fixed")]
    ly = H - 172
    for colour, text in legend:
        out.append(f'<circle cx="52" cy="{f(ly-4)}" r="3" fill="{colour}"/>'
                   f'<text x="66" y="{f(ly)}" font-size="12.5" fill="{INK}" '
                   f'opacity="0.68" letter-spacing="0.6">{text}</text>')
        ly += 19
    for i, line in enumerate([
            "SGP4 AGAINST THE GLOBE&#8217;S OWN CLOCK &#183; TWO SIDEREAL DAYS "
            f"EVERY {f(LOOP,0)} SECONDS",
            "THE FRAME IS EARTH-FIXED: GROUND TRACKS STAND STILL AND THE ORBIT "
            "PLANES TURN",
            "ORBIT RADII COMPRESSED FOR DISPLAY"]):
        out.append(f'<text x="44" y="{H-54+i*17}" font-size="11" '
                   f'fill="{INK}" opacity="{0.34 - i*0.06:.2f}" '
                   f'letter-spacing="1.3">{line}</text>')
    out.append("</g>")
    return "".join(out)

LAND_URL = ("https://cdn.jsdelivr.net/npm/world-atlas@2/land-110m.json")

def land_file(path):
    """Natural Earth 110m land, fetched once and kept next to this script."""
    if not os.path.exists(path):
        import urllib.request
        print(f"fetching {LAND_URL}")
        urllib.request.urlretrieve(LAND_URL, path)
    return path

HEADER = f"""<!--
  Terramenta - the ephemeris layer, showing what it draws.

  Generated by docs/satellites.py; do not edit by hand.  Nothing here is drawn
  freehand: the orbits are Keplerian, propagated against a clock of {f(LOOP,0)}
  seconds per {SIM_DAYS} sidereal days, rotated into the Earth-fixed frame and
  projected orthographically, with the hidden halves of every ground track and
  coastline cut against the limb.  Orbital radii are compressed by a power law
  so that a geostationary ring and a space station fit one picture.

  Coastlines: Natural Earth 110m land (public domain), via world-atlas.
-->"""

HERE = os.path.dirname(os.path.abspath(__file__))

def main():
    land = land_file(sys.argv[1] if len(sys.argv) > 1
                     else os.path.join(HERE, "land-110m.json"))
    out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(HERE, "satellites.svg")
    back_s, front_s = rings(STARLINK[:12], STEEL, 0.6, 0.30, 0.15)
    back_g, front_g = rings(GPS[::2], VIOLET, 0.9, 0.40, 0.20)
    back_e, front_e = rings(GEO[:1], "#ffd28a", 0.9, 0.40, 0.24)
    back_t, front_t = rings(STATIONS, CYAN, 1.0, 0.45, 0.24)
    molniya = "".join(ellipse_orbit(s, PINK, 1.0, 0.40) for s in MOLNIYA)
    svg = (HEADER +
           f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" '
           f'width="{W}" height="{H}" font-family="ui-monospace, SFMono-Regular, '
           f'Menlo, Consolas, monospace">'
           + defs()
           + f'<rect width="{W}" height="{H}" fill="url(#space)"/>'
           + starfield()
           + back_s + back_g + back_e + molniya + back_t
           + globe(land)
           + equator()
           + front_s + front_g + front_e + front_t
           + satellites()
           + hud()
           + inset()
           + '</svg>')
    open(out, "w").write(svg)
    print(f"{out}: {len(svg)/1024:.0f} KB")

if __name__ == "__main__":
    main()
