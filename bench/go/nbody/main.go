package main

import (
	"fmt"
	"math"
)

type Body struct{ x, y, z, vx, vy, vz, mass float64 }

func advance(bodies []Body, dt float64) {
	n := len(bodies)
	for i := 0; i < n; i++ {
		for j := i + 1; j < n; j++ {
			dx := bodies[i].x - bodies[j].x
			dy := bodies[i].y - bodies[j].y
			dz := bodies[i].z - bodies[j].z
			d2 := dx*dx + dy*dy + dz*dz
			mag := dt / (d2 * math.Sqrt(d2))
			mi := bodies[i].mass * mag
			mj := bodies[j].mass * mag
			bodies[i].vx -= dx * mj
			bodies[i].vy -= dy * mj
			bodies[i].vz -= dz * mj
			bodies[j].vx += dx * mi
			bodies[j].vy += dy * mi
			bodies[j].vz += dz * mi
		}
	}
	for i := range bodies {
		b := &bodies[i]
		b.x += dt * b.vx
		b.y += dt * b.vy
		b.z += dt * b.vz
	}
}

func energy(bodies []Body) float64 {
	e := 0.0
	for i := range bodies {
		b := bodies[i]
		e += 0.5 * b.mass * (b.vx*b.vx + b.vy*b.vy + b.vz*b.vz)
		for j := i + 1; j < len(bodies); j++ {
			c := bodies[j]
			dx, dy, dz := b.x-c.x, b.y-c.y, b.z-c.z
			e -= b.mass * c.mass / math.Sqrt(dx*dx+dy*dy+dz*dz)
		}
	}
	return e
}

func main() {
	pi := 3.141592653589793
	solarMass := 4.0 * pi * pi
	days := 365.24
	bodies := []Body{
		{0, 0, 0, 0, 0, 0, solarMass},
		{4.84143144246472090e+00, -1.16032004402742839e+00, -1.03622044471123109e-01, 1.66007664274403694e-03 * days, 7.69901118419740425e-03 * days, -6.90460016972063023e-05 * days, 9.54791938424326609e-04 * solarMass},
		{8.34336671824457987e+00, 4.12479856412430479e+00, -4.03523417114321381e-01, -2.76742510726862411e-03 * days, 4.99852801234917238e-03 * days, 2.30417297573763929e-05 * days, 2.85885980666130812e-04 * solarMass},
		{1.28943695621391310e+01, -1.51111514016986312e+01, -2.23307578892655734e-01, 2.96460137564761618e-03 * days, 2.37847173959480950e-03 * days, -2.96589568540237556e-05 * days, 4.36624404335156298e-05 * solarMass},
		{1.53796971148509165e+01, -2.59193146099879641e+01, 1.79258772950371181e-01, 2.68067772490389322e-03 * days, 1.62824170038242295e-03 * days, -9.51592254519715870e-05 * days, 5.15138902046611451e-05 * solarMass},
	}
	var px, py, pz float64
	for _, b := range bodies {
		px += b.vx * b.mass
		py += b.vy * b.mass
		pz += b.vz * b.mass
	}
	bodies[0].vx = -px / solarMass
	bodies[0].vy = -py / solarMass
	bodies[0].vz = -pz / solarMass
	fmt.Println(int64(math.Round(energy(bodies) * 1e9)))
	for i := 0; i < 5000000; i++ {
		advance(bodies, 0.01)
	}
	fmt.Println(int64(math.Round(energy(bodies) * 1e9)))
}
