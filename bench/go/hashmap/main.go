package main

import (
	"fmt"
	"strconv"
)

func main() {
	n := 1000000
	m := map[int]int{}
	for i := 0; i < n; i++ {
		m[(i*7919)%2000003] = i
	}
	sum := 0
	for i := 0; i < n; i++ {
		if v, ok := m[i]; ok {
			sum += v
		}
	}
	fmt.Println(len(m), sum)

	k := 200000
	s := map[string]int{}
	for i := 0; i < k; i++ {
		s[strconv.Itoa(i)] = i
	}
	hits := 0
	for i := 0; i < k*2; i++ {
		if _, ok := s[strconv.Itoa(i)]; ok {
			hits++
		}
	}
	fmt.Println(len(s), hits)
}
