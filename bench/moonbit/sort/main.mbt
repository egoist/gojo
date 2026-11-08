// Sort 5e6 integers from a 64-bit LCG, same as ../gojo/sort.gojo.
// The multiply is done in Int64 so the sequence matches Go (where `int` is
// 64-bit and the product does not overflow).

///|
fn main {
  let n = 5000000
  let v : Array[Int64] = Array(capacity=n)
  let mut seed = 42L
  for i = 0; i < n; i = i + 1 {
    seed = (seed * 1103515245L + 12345L) % 2147483648L
    v.push(seed)
  }
  v.sort()
  let mut ok = true
  for i = 1; i < n; i = i + 1 {
    if v[i - 1] > v[i] {
      ok = false
    }
  }
  println("\{ok} \{v[0]} \{v[n / 2]} \{v[n - 1]}")
}
