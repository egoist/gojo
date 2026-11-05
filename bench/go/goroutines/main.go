package main

import "fmt"

func main() {
	n := 100000
	start := make(chan bool)
	prev := make(chan int)
	first := prev
	for i := 0; i < n; i++ {
		next := make(chan int)
		input, output := prev, next
		go func() {
			<-start
			v := <-input
			output <- v + 1
		}()
		prev = next
	}
	close(start)
	first <- 0
	fmt.Println(<-prev)
}
