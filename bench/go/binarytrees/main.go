package main

import "fmt"

type Node struct{ left, right *Node }

func make_(d int) *Node {
	if d == 0 {
		return &Node{}
	}
	return &Node{make_(d - 1), make_(d - 1)}
}

func (n *Node) check() int {
	c := 1
	if n.left != nil {
		c += n.left.check()
	}
	if n.right != nil {
		c += n.right.check()
	}
	return c
}

func main() {
	n, minDepth := 18, 4
	maxDepth := max(minDepth+2, n)
	stretch := maxDepth + 1
	fmt.Println("stretch tree of depth", stretch, "check:", make_(stretch).check())
	long := make_(maxDepth)
	for d := minDepth; d <= maxDepth; d += 2 {
		iterations := 1 << (maxDepth - d + minDepth)
		sum := 0
		for i := 0; i < iterations; i++ {
			sum += make_(d).check()
		}
		fmt.Println(iterations, "trees of depth", d, "check:", sum)
	}
	fmt.Println("long lived tree of depth", maxDepth, "check:", long.check())
}
