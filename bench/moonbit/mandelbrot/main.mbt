// Mandelbrot n=2000, 50 iterations, same algorithm as ../gojo/mandelbrot.gojo.

///|
fn main {
  let n = 2000
  let fn_ = n.to_double()
  let mut inside = 0L
  for y = 0; y < n; y = y + 1 {
    let ci = 2.0 * y.to_double() / fn_ - 1.0
    for x = 0; x < n; x = x + 1 {
      let cr = 2.0 * x.to_double() / fn_ - 1.5
      let mut zr = 0.0
      let mut zi = 0.0
      let mut escaped = false
      for i = 0; i < 50; i = i + 1 {
        let tr = zr * zr - zi * zi + cr
        zi = 2.0 * zr * zi + ci
        zr = tr
        if zr * zr + zi * zi > 4.0 {
          escaped = true
          break
        }
      }
      if !escaped {
        inside += 1L
      }
    }
  }
  println(inside)
}
