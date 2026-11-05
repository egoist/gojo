package main

import (
	"fmt"
	"slices"
)

func main() {
	n := 5000000
	v := make([]int, 0, n)
	seed := 42
	for i := 0; i < n; i++ {
		seed = (seed*1103515245 + 12345) % 2147483648
		v = append(v, seed)
	}
	slices.Sort(v)
	ok := true
	for i := 1; i < n; i++ {
		if v[i-1] > v[i] {
			ok = false
		}
	}
	fmt.Println(ok, v[0], v[n/2], v[n-1])
}
