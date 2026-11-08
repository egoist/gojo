// Integer-key and string-key maps, same sizes as ../gojo/hashmap.gojo.
// Keys use Int64 because i*7919 for i near 1e6 does not fit in Int32, matching
// Go/Gojo 64-bit `int` arithmetic.

///|
fn main {
  let n = 1000000
  let m : Map[Int64, Int64] = Map([])
  for i = 0; i < n; i = i + 1 {
    let ii = i.to_int64()
    m[ii * 7919L % 2000003L] = ii
  }
  let mut sum = 0L
  for i = 0; i < n; i = i + 1 {
    match m.get(i.to_int64()) {
      Some(v) => sum += v
      None => ()
    }
  }
  println("\{m.length()} \{sum}")

  let k = 200000
  let s : Map[String, Int64] = Map([])
  for i = 0; i < k; i = i + 1 {
    s[i.to_string()] = i.to_int64()
  }
  let mut hits = 0
  for i = 0; i < k * 2; i = i + 1 {
    if s.contains(i.to_string()) {
      hits += 1
    }
  }
  println("\{s.length()} \{hits}")
}
