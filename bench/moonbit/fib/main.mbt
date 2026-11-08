// Naive recursive Fibonacci, same algorithm as ../gojo/fib.gojo and ../go/fib.
// Int64 matches Go/Gojo `int` on 64-bit hosts. println of the result keeps the
// work live (no DCE).

///|
fn fib(n : Int64) -> Int64 {
  if n < 2 {
    n
  } else {
    fib(n - 1) + fib(n - 2)
  }
}

///|
fn main {
  println(fib(40L))
}
