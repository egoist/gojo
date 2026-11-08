// Fannkuch-redux n=10, same algorithm as ../gojo/fannkuch.gojo.
// Permutation values fit in Int32; the Go/Gojo versions store them in `int`.

///|
fn main {
  let n = 10
  let perm : Array[Int64] = Array(capacity=n)
  let perm1 : Array[Int64] = Array(capacity=n)
  let count : Array[Int64] = Array(capacity=n)
  for i = 0; i < n; i = i + 1 {
    perm.push(0L)
    perm1.push(i.to_int64())
    count.push(0L)
  }
  let mut max_flips = 0L
  let mut checksum = 0L
  let mut perm_count = 0L
  let mut r = n
  for ;; {
    while r != 1 {
      count[r - 1] = r.to_int64()
      r -= 1
    }
    for i = 0; i < n; i = i + 1 {
      perm[i] = perm1[i]
    }
    let mut flips = 0L
    let mut k = perm[0]
    while k != 0L {
      let mut lo = 0
      let mut hi = k.to_int()
      while lo < hi {
        let t = perm[lo]
        perm[lo] = perm[hi]
        perm[hi] = t
        lo += 1
        hi -= 1
      }
      flips += 1
      k = perm[0]
    }
    if flips > max_flips {
      max_flips = flips
    }
    if perm_count % 2L == 0L {
      checksum += flips
    } else {
      checksum -= flips
    }
    let mut done = false
    for ;; {
      if r == n {
        done = true
        break
      }
      let p0 = perm1[0]
      for i = 0; i < r; i = i + 1 {
        perm1[i] = perm1[i + 1]
      }
      perm1[r] = p0
      count[r] -= 1L
      if count[r] > 0L {
        break
      }
      r += 1
    }
    if done {
      break
    }
    perm_count += 1L
  }
  println(checksum)
  println("Pfannkuchen( \{n} ) = \{max_flips}")
}
