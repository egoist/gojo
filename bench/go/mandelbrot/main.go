package main

import "fmt"

func main() {
	n := 2000
	fn := float64(n)
	inside := 0
	for y := 0; y < n; y++ {
		ci := 2.0*float64(y)/fn - 1.0
		for x := 0; x < n; x++ {
			cr := 2.0*float64(x)/fn - 1.5
			zr, zi := 0.0, 0.0
			escaped := false
			for i := 0; i < 50; i++ {
				tr := zr*zr - zi*zi + cr
				zi = 2.0*zr*zi + ci
				zr = tr
				if zr*zr+zi*zi > 4.0 {
					escaped = true
					break
				}
			}
			if !escaped {
				inside++
			}
		}
	}
	fmt.Println(inside)
}
