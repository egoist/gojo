// Spectral-norm n=2000, same algorithm as ../gojo/spectralnorm.gojo.

///|
fn a(i : Int32, j : Int32) -> Double {
  let ii = i.to_int64()
  let jj = j.to_int64()
  1.0 / ((ii + jj) * (ii + jj + 1L) / 2L + ii + 1L).to_double()
}

///|
fn mul_av(v : Array[Double], out : Array[Double]) -> Unit {
  let n = v.length()
  for i = 0; i < n; i = i + 1 {
    let mut s = 0.0
    for j = 0; j < n; j = j + 1 {
      s += a(i, j) * v[j]
    }
    out[i] = s
  }
}

///|
fn mul_atv(v : Array[Double], out : Array[Double]) -> Unit {
  let n = v.length()
  for i = 0; i < n; i = i + 1 {
    let mut s = 0.0
    for j = 0; j < n; j = j + 1 {
      s += a(j, i) * v[j]
    }
    out[i] = s
  }
}

///|
fn mul_at_av(
  v : Array[Double],
  out : Array[Double],
  tmp : Array[Double],
) -> Unit {
  mul_av(v, tmp)
  mul_atv(tmp, out)
}

///|
fn main {
  let n = 2000
  let u : Array[Double] = Array(capacity=n)
  let v : Array[Double] = Array(capacity=n)
  let tmp : Array[Double] = Array(capacity=n)
  for i = 0; i < n; i = i + 1 {
    u.push(1.0)
    v.push(0.0)
    tmp.push(0.0)
  }
  for i = 0; i < 10; i = i + 1 {
    mul_at_av(u, v, tmp)
    mul_at_av(v, u, tmp)
  }
  let mut vbv = 0.0
  let mut vv = 0.0
  for i = 0; i < n; i = i + 1 {
    vbv += u[i] * v[i]
    vv += v[i] * v[i]
  }
  println(((vbv / vv).sqrt() * 1.0e9).round().to_int64())
}
