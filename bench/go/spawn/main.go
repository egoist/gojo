package main

import "fmt"

func main() {
	done := make(chan int, 1000)
	total := 0
	for round := 0; round < 1000; round++ {
		for i := 0; i < 1000; i++ {
			go func(v int) { done <- v }(round + i)
		}
		for j := 0; j < 1000; j++ {
			total += <-done
		}
	}
	fmt.Println(total)
}
