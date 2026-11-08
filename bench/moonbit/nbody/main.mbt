// N-body (5 bodies, 5e6 steps), same algorithm as ../gojo/nbody.gojo.

///|
struct Body {
  mut x : Double
  mut y : Double
  mut z : Double
  mut vx : Double
  mut vy : Double
  mut vz : Double
  mass : Double
}

///|
fn advance(bodies : Array[Body], dt : Double) -> Unit {
  let n = bodies.length()
  for i = 0; i < n; i = i + 1 {
    for j = i + 1; j < n; j = j + 1 {
      let dx = bodies[i].x - bodies[j].x
      let dy = bodies[i].y - bodies[j].y
      let dz = bodies[i].z - bodies[j].z
      let d2 = dx * dx + dy * dy + dz * dz
      let mag = dt / (d2 * d2.sqrt())
      let mi = bodies[i].mass * mag
      let mj = bodies[j].mass * mag
      bodies[i].vx -= dx * mj
      bodies[i].vy -= dy * mj
      bodies[i].vz -= dz * mj
      bodies[j].vx += dx * mi
      bodies[j].vy += dy * mi
      bodies[j].vz += dz * mi
    }
  }
  for i = 0; i < n; i = i + 1 {
    bodies[i].x += dt * bodies[i].vx
    bodies[i].y += dt * bodies[i].vy
    bodies[i].z += dt * bodies[i].vz
  }
}

///|
fn energy(bodies : Array[Body]) -> Double {
  let n = bodies.length()
  let mut e = 0.0
  for i = 0; i < n; i = i + 1 {
    let b = bodies[i]
    e += 0.5 * b.mass * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz)
    for j = i + 1; j < n; j = j + 1 {
      let c = bodies[j]
      let dx = b.x - c.x
      let dy = b.y - c.y
      let dz = b.z - c.z
      e -= b.mass * c.mass / (dx * dx + dy * dy + dz * dz).sqrt()
    }
  }
  e
}

///|
fn main {
  let pi = 3.141592653589793
  let solar_mass = 4.0 * pi * pi
  let days = 365.24
  let bodies : Array[Body] = [
    Body::{
      x: 0.0,
      y: 0.0,
      z: 0.0,
      vx: 0.0,
      vy: 0.0,
      vz: 0.0,
      mass: solar_mass,
    },
    Body::{
      x: 4.84143144246472090e+00,
      y: -1.16032004402742839e+00,
      z: -1.03622044471123109e-01,
      vx: 1.66007664274403694e-03 * days,
      vy: 7.69901118419740425e-03 * days,
      vz: -6.90460016972063023e-05 * days,
      mass: 9.54791938424326609e-04 * solar_mass,
    },
    Body::{
      x: 8.34336671824457987e+00,
      y: 4.12479856412430479e+00,
      z: -4.03523417114321381e-01,
      vx: -2.76742510726862411e-03 * days,
      vy: 4.99852801234917238e-03 * days,
      vz: 2.30417297573763929e-05 * days,
      mass: 2.85885980666130812e-04 * solar_mass,
    },
    Body::{
      x: 1.28943695621391310e+01,
      y: -1.51111514016986312e+01,
      z: -2.23307578892655734e-01,
      vx: 2.96460137564761618e-03 * days,
      vy: 2.37847173959480950e-03 * days,
      vz: -2.96589568540237556e-05 * days,
      mass: 4.36624404335156298e-05 * solar_mass,
    },
    Body::{
      x: 1.53796971148509165e+01,
      y: -2.59193146099879641e+01,
      z: 1.79258772950371181e-01,
      vx: 2.68067772490389322e-03 * days,
      vy: 1.62824170038242295e-03 * days,
      vz: -9.51592254519715870e-05 * days,
      mass: 5.15138902046611451e-05 * solar_mass,
    },
  ]
  let mut px = 0.0
  let mut py = 0.0
  let mut pz = 0.0
  for i = 0; i < bodies.length(); i = i + 1 {
    let b = bodies[i]
    px += b.vx * b.mass
    py += b.vy * b.mass
    pz += b.vz * b.mass
  }
  bodies[0].vx = -px / solar_mass
  bodies[0].vy = -py / solar_mass
  bodies[0].vz = -pz / solar_mass
  println((energy(bodies) * 1.0e9).round().to_int64())
  for i = 0; i < 5000000; i = i + 1 {
    advance(bodies, 0.01)
  }
  println((energy(bodies) * 1.0e9).round().to_int64())
}
