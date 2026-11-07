# fmt

Formatting and printing. The functions here are built into the compiler.

`print`, `println`, `eprint` and `eprintln` (to standard error) need no
import. They, `fmt.sprint` and `fmt.sprintln` (which return the text) take
any number of values of any type but functions, separated by spaces:

```kite
println("x =", 1, 2.5, true)          // x = 1 2.5 true
println(P{x: 1, y: "q"}, [1, 2])      // P{x: 1, y: "q"} [1, 2]
let s = fmt.sprint("id", 7)           // "id 7"
```

Strings print as they are at the top level and quoted inside other values;
floats print in the shortest form that reads back the same; `?T` values
print as `none` or `some(x)`; maps keep their insertion order. A `rune` is
an `i32` and prints as a number: `s.pushRune(r)` adds it to a string.

`fmt.format` returns a string, and `fmt.printf` prints it, from a template
with `{}` placeholders:

```kite
fmt.format("{} has {} items", name, n)
fmt.printf("{:>8}|{:<4}|{:^7}|\n", "r", 1, "mid")   //        r|1   |  mid  |
fmt.format("{:08.3}", -3.14159)                      // -003.142
fmt.format("{:x} {:X} {:o} {:b}", 255, 255, 8, 5)    // ff FF 10 101

{}        the value, as `print` shows it
{:8}      at least 8 characters wide (numbers right-aligned, the rest left)
{:<8} {:>8} {:^8}   left, right, centered;   {:*^8}   with `*` as the fill
{:08}     numbers padded with zeros after the sign
{:.2}     a float with 2 digits after the point
{:x} {:X} {:o} {:b}   an integer in hex, octal or binary
{{ }}     literal braces
```

The template must be a string literal: the compiler checks the number of
arguments and that each specification fits its argument's type.

