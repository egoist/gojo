// Binary-trees, same algorithm and n=18 as ../gojo/binarytrees.gojo.

///|
struct Node {
  left : Node?
  right : Node?
}

///|
fn make(depth : Int32) -> Node {
  if depth == 0 {
    Node::{ left: None, right: None, }
  } else {
    Node::{ left: Some(make(depth - 1)), right: Some(make(depth - 1)), }
  }
}

///|
fn check(n : Node) -> Int64 {
  let mut c = 1L
  if n.left is Some(l) {
    c += check(l)
  }
  if n.right is Some(r) {
    c += check(r)
  }
  c
}

///|
fn main {
  let n = 18
  let min_depth = 4
  let max_depth = if min_depth + 2 > n { min_depth + 2 } else { n }
  let stretch = max_depth + 1
  println("stretch tree of depth \{stretch} check: \{check(make(stretch))}")
  let long = make(max_depth)
  let mut d = min_depth
  while d <= max_depth {
    let iterations = 1 << (max_depth - d + min_depth)
    let mut sum = 0L
    for i = 0; i < iterations; i = i + 1 {
      sum += check(make(d))
    }
    println("\{iterations} trees of depth \{d} check: \{sum}")
    d += 2
  }
  println("long lived tree of depth \{max_depth} check: \{check(long)}")
}
