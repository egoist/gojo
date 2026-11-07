# Kite

**English:** [A tour of Kite](docs/tour.md) · [Standard library reference](docs/std/README.md)

Kite 是一门系统编程语言：保留 Go 的简单和编译速度，用 Rust 式的所有权去掉 GC 和 nil，并吸收 Zig 的错误联合、`errdefer`、显式回绕运算和零依赖交叉编译。

这个仓库是按 [设计文档](docs/design.md) 实现的编译器原型：编译器用 Rust 编写（约 1.9 万行，无第三方依赖），C 运行时约 4000 行，Kite 标准库约 3400 行。

```go
use std/os

struct Config {
    name String // 必填
    port int = 8080 // 默认值
    proxy ?String // 可选，默认 none
}

fn load(path str) Config ! os.Error {
    let text = try os.readText(path)
    return Config{name: text.trim().toString()}
}

fn main() {
    let cfg = load("app.toml") catch e {
        eprintln("using defaults:", e)
        Config{name: "app"}
    }
    var hits = 0
    for i in 0..10 { hits += i }
    println(cfg.name, cfg.port, hits)
}
```

## 包与导入

没有 `package` 声明：一个目录就是一个包，包名就是目录名；`kite run` 的入口包里放 `main` 函数。导入用 `use`，路径不加引号：

```go
use std/{fmt, os, time}        // 标准库：都在 std 下，一行可以导入几个
use ./geom                     // 本项目的包：相对当前包目录的路径
use ../shared/util as u        // 嵌套路径，as 重命名
use ./geom.{Vec2, Shape}       // 把名字直接引入作用域，不再绑定 geom
```

看路径开头就知道是标准库（`std/`）还是项目内的包（`./`），不会重名；`use std/os` 绑定的名字是 `os`。`print`/`println`/`eprint`/`eprintln` 在 prelude 里，hello world 不需要任何导入；`fmt` 只剩 `fmt.sprint`、`fmt.format` 这类格式化函数。未使用的导入（包括 `.{...}` 里的名字）是编译错误。

## 方法

方法和类型写在一起，接收者是显式的 `self`：

```go
struct Counter {
    n int = 0

    fn new(start int) Counter {        // 没有 self：关联函数，Counter.new(10)
        return Counter{n: start}
    }

    fn value(self) int {               // 借用
        return self.n
    }

    fn bump(self inout) {              // 可变借用
        self.n += 1
    }
}

extend Counter {                         // 补充方法，例如放在同一个包的另一个文件里
    fn reset(self inout) { self.n = 0 }
}

interface Bumper {
    fn bump(self inout)                // 接口方法也是同一种写法
}
```

## 安装

支持 macOS 和 Linux（x86-64、arm64），**不支持 Windows**（运行时依赖 POSIX）。Kite 编译到 C，所以需要一个 C 编译器：`PATH` 上的 `cc`、`clang`、`gcc` 或 `zig`（按这个顺序查找；macOS 上 `xcode-select --install`；交叉编译用 `zig`）。

发布包是一个静态链接的 `kite` 可执行文件（标准库和运行时都编译在里面），加上文档和示例：

```sh
curl -fsSL https://github.com/OWNER/kite/releases/latest/download/install.sh | KITE_RELEASES=https://github.com/OWNER/kite/releases sh
sh scripts/install.sh --archive dist/kite-0.1.0-macos-arm64.tar.gz    # 或者装本地的包
```

装到 `~/.kite`（`KITE_HOME` 可改），`kite version` 显示版本。从源码构建只需要 Rust 工具链：`cargo build --release`。

## 快速开始

```sh
cargo build --release
./target/release/kite run examples/hello.kite
./target/release/kite run examples/json.kite
./target/release/kite run examples/shapes        # 一个目录 = 一个包，含子包 geom
./target/release/kite test examples/shapes       # 运行 test "..." { } 块
```

| 命令 | 作用 |
| --- | --- |
| `kite run <文件或目录> [参数...]` | 编译并运行 |
| `kite build <文件或目录> [-o 输出]` | 生成原生二进制 |
| `kite check <文件或目录>` | 只做类型检查和借用检查 |
| `kite test <文件或目录>` | 运行 `test` 块，每个测试在独立进程中执行，panic 不影响其他测试 |
| `kite fmt [--check] [文件或目录...]` | 就地格式化（默认当前目录）；`--check` 只列出没格式化的文件，有则退出码 1 |
| `kite doc [模块或路径] [--out 目录]` | Markdown 格式的 API 文档：标准库索引、一个标准库模块或一个包；`--out` 把整个标准库写到目录里（即 [`docs/std`](docs/std/README.md)） |
| `kite emit-c <文件或目录>` | 输出生成的 C 代码 |
| `kite version` | 版本、提交和目标平台 |

选项：`--release`（`-O2`，溢出检查仍然开启）、`--cc "<编译器 参数>"`、`--target <三元组>`（用 `zig cc` 交叉编译，例如 `--target x86_64-linux-musl` 直接得到静态 Linux 二进制）、`-v`（打印各阶段耗时）。

## 安全保证，以及它们怎样被保证

所有检查都只看函数签名和单个函数体，没有跨函数推断，也没有生命周期标注。每一条都有对应的编译错误测试（`tests/errors/`，共 83 个）。

| 保证 | 机制 | 典型报错 |
| --- | --- | --- |
| 没有 nil | 只有 `?T` 能为空，用前必须解开 | `` undefined: `nil` `` / `` `none` is not a value of type `int` `` |
| 没有释放后使用 | move 语义 + "可能已移动"数据流分析，循环跑两遍以发现上一轮的 move | `` use of `s` after it was moved `` / `moved here in a previous iteration of the loop` |
| 引用不会悬垂 | 引用只能作参数、投影（`yield`）或块级绑定；视图（`str`、`[]T`）只存在于变量里，不能进字段、集合、盒子、接口值、泛型类型或逃逸闭包（推断出的类型也一样）；赋给外层变量的视图，被借的值必须活得至少一样长；锁里的数据的视图不能离开 `with` 块 | `this temporary value is dropped at the end of the statement` / `` returns a view of `s` `` / `` `s` does not live long enough `` |
| 迭代器不失效 | `for`/`with`/`if let` 绑定在块内持有借用 | `` cannot modify `v` while it is borrowed by `x` `` |
| 独占的可变借用 | 同一次调用里的实参互斥检查，`inout` 在调用处必须写 `&` | `` `a` is used by two arguments of the same call `` |
| 编译期没有数据竞争 | `send` 按类型结构推导；`rc`/`Cell`/裸指针不能跨 goroutine；共享可变状态只能经过 `Mutex` | `` argument of type `rc[int]` cannot be sent to another goroutine `` / `a task cannot modify the captured variable` |
| 错误必须处理 | `T ! E` 不处理就是编译错误，丢弃要写 `_ = f()` | `` result of type `! String` is not handled `` |
| 穷尽匹配 | 基于 usefulness 算法，报出漏掉的具体模式 | `` `match` is not exhaustive: `Rect(_, _)` is not covered `` |
| 运行时检查 | 整数溢出、下标越界、除零、移位越界、数值转换越界、`unwrap` 失败都会 panic，报出调用处位置 | `panic: integer overflow in addition` |

此外：`let` 不可变、同一函数内禁止遮蔽、未使用的变量和导入报错、`copy` 类型不能含需要释放的字段、无限大小的递归类型报错、调用 `extern` 函数和解引用裸指针必须在 `unsafe` 块中。

生成代码的内存正确性单独验证过：全部测试程序在 AddressSanitizer + UndefinedBehaviorSanitizer 下无报错（`cargo test -- --ignored`），macOS `leaks` 检测无泄漏。这些检查本身也被系统地攻击过：`kite-fuzz` 对编译器做变异模糊测试和差分测试（见下面“测试”），另有一组专门构造的绕过借用检查的程序，找到的漏洞都已修复并加进了 `tests/errors/`。

## 已实现的语言特性

- **基础**：`let`/`var`、`for x in xs`、`for i in 0..n`、`for cond {}`、`for {}`、循环标签（`outer: for ...`，`break outer`）、`if`/`match`/块作为表达式、`defer`（块级）、`errdefer`、`const`、类型别名和新类型（`type Celsius f64`，底层是数字时保留运算符）、`(x) => x * 2` 短闭包。
- **类型**：全部标量、`String`/`str`、`Vec[T]`/`[]T`、`[N]T`、`Map[K, V]`、`?T`、`T ! E`、`Box`、`rc`/`weak`、`arc`、`Cell`、`Mutex`、`chan T`、函数类型、`*T`。
- **方法**：写在 `struct`/`enum` 的大括号里，第一个参数 `self` 带接收者模式；关联函数 `Stack[String].new()`（泛型参数可由期望类型推断）；`extend T { ... }` 补充方法，标准库还用它给内置类型及其特定实例（如 `extend []String`）加方法。
- **结构体与枚举**：字段默认值（缺少必填字段是编译错误）、`copy struct`、包内私有类型自动推导 copy、和类型枚举、`.Variant` 简写、struct 嵌入（字段和方法提升）。
- **所有权**：三种参数模式（借用 / `inout` / `sink`）、确定性释放（作用域逆序、`drop` 方法、`drop(x)`）、部分移动、条件移动（只在需要时插入标志位）、`.clone()`。
- **投影**：`yield &T` / `yield inout T` / `yield ?&T` 函数；`v[i]`、`m.get(k)`、`m.entry(k)`、`mu.lock()` 都是投影，借出的引用带不出块。绑定到 `let` 的引用和视图在最后一次使用之后就不再占用被借的值。解析器这类结构用 `Span` 存区间，不需要自引用。
- **自定义迭代**：游标协议（`cursor` + `next`/`nextMut`，`zip` 和归并只是同时推进几个游标）和推送协议（`each`/`eachMut`，适合树这样的递归结构）；两种协议里 `break`、`continue`、`return`、`try`、带标签的跳转都照常工作。
- **错误处理**：`try`、`try x else Wrap`、`catch e { ... }`、`match` 解构 `ok`/`err`、`?T` 上的 `try`、`??`、`?.`。
- **接口**：结构化隐式满足、接口值（堆上盒子 + 虚表）、借用接口参数（胖指针，不分配）、`fn bump(self inout) int` 这种可变接收者方法、`match` 按具体类型分支、嵌入接口、带类型集的约束接口（`~int | ~f64`）。
- **泛型**：泛型函数/结构体/枚举、方法自带类型参数、`where` 条件方法、内置能力约束（`copy`、`clone`、`default`、`send`、`comparable`、`ordered`、`hash`）、局部双向推断（含闭包返回类型）、整数常量泛型（`Mat[R, C]`，维度不匹配是编译错误）。自定义 `equal`/`hash`/`compare` 方法后可以作 Map 键或排序。
- **并发**：M:N 调度的有栈协程（10 万个 goroutine 在 Linux 上约 440 MB），阻塞的 channel、锁、`time.sleep`、网络操作只挂起 goroutine；抢占（有其他 goroutine 在等时，运行 10 ms 后让出 CPU；8 个基准的周期数几何平均没有变化，单项最多 +5%）；C 调用和文件 I/O 期间把 CPU 让给其他线程；`task.setMaxProcs`、`task.stats()`；`go`、带缓冲/无缓冲 channel（发送即转移所有权）、`close`、`for x in ch`、`select`（分支写法同 `match`：`let x = <-ch => ...`、`ch <- v => ...`、`_ => ...`）、`arc(Mutex(x))`、`Mutex(&x)`（锁借用外部数据，用在 `task.group` 里）、死锁检测。
- **结构化并发与取消**：`task.group`（任务借用组外数据、拿走组体里的值，首个错误作为组的结果），取消沿任务树传播（`g.cancel()`、失败的子任务、`task.timeout(d, body)` 的期限），被取消任务里的阻塞操作立即返回，`try task.check()` 检查点。
- **网络**：`net.listen`/`accept`/`dial`，`Conn` 的读写不阻塞线程、可被取消。[`examples/http`](examples/http) 是一个带请求超时、客户端断开即取消、停机时取消在途请求的 HTTP/1.1 服务器。
- **unsafe 与 FFI**：`extern "C" lib "libz" { ... }` 运行时 `dlopen`，构建时不需要 C 头文件；不写 `lib` 时直接调用 libc；`extern "C" static` 静态链接（静态的 musl 二进制里也能用）；`extern "C" fn` 导出 Kite 函数；`unsafe send struct` 由作者担保跨线程安全。没有 C 表示的类型（`String`、函数值等）不能出现在 `extern` 签名里。
- **标准库**：prelude（Option/Result/字符串/切片/Vec/Map/`Set` 方法、`Arena[T]` + 带代数的 `Id[T]`、`Hasher`）、`fmt`（含编译时检查模板的 `fmt.format`）、`io`（统一的错误类型和 `Reader`/`Writer`）、`bufio`、`os`（文件、目录、环境变量、子进程）、`time`（单调时钟、日期、格式化和解析）、`json`、`net`、`http`（服务端和客户端）、`task`、`math`、`strings`。内置类型的方法签名用 Kite 写在 `std/prelude.kite`，其中 `map`/`filter`/`fold`/`join`/`Arena`/`Set` 以及 `bufio`、`json`、`http` 等模块本身就是 Kite 代码。各模块的接口见 [`docs/spec.md`](docs/spec.md) 第 12 节；[`examples/todo`](examples/todo)（JSON API 服务）和 [`examples/grep`](examples/grep)（命令行工具）只用标准库写成。

`tests/run/` 下的程序覆盖了以上全部特性，可以当作语言示例阅读。精确的语言规则见 [`docs/spec.md`](docs/spec.md)，每一节都列出了覆盖它的测试。

## 实现结构

```
src/lexer.rs      词法（Go 式自动分号）
src/parser.rs     递归下降语法分析
src/check/        声明收集、类型解析与能力规则（mod.rs）
                  函数体类型检查 → 类型化 IR（body.rs）
                  模式、match、穷尽性（pattern.rs）
src/borrowck.rs   所有权与借用检查（逐函数、单遍 + 循环两遍；外加两个全程序检查：
                  不能存储视图的值、保留类型参数的泛型函数）
src/codegen.rs    IR → C：单态化、析构插入、虚表、闭包、内建函数
src/lib.rs        前端的库接口（驱动程序和模糊测试共用）；src/main.rs 是 `kite` 命令
src/bin/kite-fuzz 变异模糊测试、差分测试（progen.rs 生成随机程序）、借用检查器攻击（borrowgen.rs）、化简
runtime/          C 运行时：字符串、Vec、插入有序哈希表、小对象分配器、panic、FFI（kite_rt.c）；
                  M:N 调度器、上下文切换、channel、select、mutex、任务组与取消、
                  计时器、kqueue/epoll 网络轮询（kite_sched.c）
std/              用 Kite 写的标准库（编译进二进制）
```

后端生成 C 再调用系统 C 编译器。运行时库按（编译器，参数）编译一次并缓存在 `~/.cache/kite`；生成的二进制按 C 源码哈希缓存，源码不变时再次 `run` 约 10 ms。

## 性能

[`bench/`](bench/README.md) 里有 8 个常见基准（fib、binary-trees、nbody、spectral-norm、fannkuch-redux、mandelbrot、hashmap、sort）的 Kite / Go / Zig 等价实现，`bench/run.sh` 会先校验三边输出一致再测量。Apple M5 上的结果：

- 运行时间（几何平均）：Kite 是 Go 的 0.78 倍、Zig ReleaseSafe 的 0.80 倍、Zig ReleaseFast 的 0.87 倍，而且溢出和下标检查都开着。binary-trees 受益于 Kite 的小对象分配器，不计它时分别是 0.87、0.94、1.03 倍。
- 8 项里 Kite 在 binary-trees、mandelbrot、hashmap、sort 上最快；fib 的差距是溢出检查本身的代价（和 Zig ReleaseSafe 持平）；nbody 和 Go 持平，比 Zig 慢。
- 编译时间：nbody 的 `kite build` 约 50 ms（其中前端约 3 ms），`go build` 97 ms，`zig build-exe` Debug 924 ms。

在一个 4 万行的合成程序上，Kite 前端耗时约 66 ms（解析 23 ms、类型检查 34 ms、借用检查 3 ms、生成 C 6 ms）。

并发（见 [bench/README.md](bench/README.md#goroutine)）：10 万个同时挂起的 goroutine 接力传值，Linux 上 Kite 0.38 s / 441 MB，Go 0.11 s / 284 MB；100 万个短命 goroutine（每批 1000 个），Kite 382 ms，Go 278 ms。

## 与设计文档的差异和改进

这些是有意的取舍（设计文档第 12 节记录了已定的决议）：

1. **后端**：原型生成 C，由 `cc` 或 `zig cc` 编译，因此天然支持交叉编译（已在 Alpine 容器中验证 `x86_64`/`aarch64` musl 静态二进制）。前端已经达到 Go 的速度量级，瓶颈在 C 编译器；自带的快速 debug 后端是下一步。
2. **泛型用单态化**，而不是字典传递。只实例化可达代码，编译时间目前不是问题；字典传递留到自研后端时再做。
3. **panic 只有 abort 一种模式**：不生成展开表，二进制更小；任何线程里的 panic 都结束整个进程，没有 `recover` 和 `task.Panic`。`kite test` 把每个测试放在单独的进程里，一个测试 panic 不影响其他测试。
4. **match 的绑定模式**：匹配一个“位置”时绑定默认是引用；如果某个分支把绑定 move 走（例如 `some(v) => v`），这个分支自动变为消耗型，被匹配的拥有值随之被移动。这样既不用写 `ref`，也能在 `sink` 接收者里写出 `unwrap`。
5. **`fn` 关键字**：函数、方法、函数类型（`fn(int) int`）和函数字面量都用 `fn`，取代 Go 的 `func`。
6. **包与导入**：去掉了 Go 式的 `package main` 和带引号的 `import "fmt"`，改为“目录即包”和 `use`（见上文“包与导入”）。这也去掉了 Go 里包名和目录名必须一致的冗余；`main` 不需要 `pub`。
7. **结构体/枚举自动获得 `clone`、`==`、`hash`**（字段都支持时）；需要自定义时写 `clone`/`equal`/`hash`/`compare` 方法。`copy` 对导出类型必须显式声明（避免给公开类型加字段时悄悄破坏下游），包内私有的非泛型类型自动推导。
8. **新增 `errdefer`**（来自 Zig）：只在函数以错误返回时执行，适合回滚。
9. **字符串字面量列表推断为 `Vec[String]`**，避免把借用视图存进集合。
10. **打印**：类型有 `toString()` 或 `message()` 方法时用它，否则按结构打印（`Config{name: "app", port: 8080}`）。
11. **Map 保持插入顺序**，迭代顺序确定。
12. **方法写在类型里**：取代 Go 的 `func (p *Point) scale(k int)`。接收者是显式的 `self`（`self` / `self inout` / `self sink`），没有 `self` 的是关联函数（`Point.new()`），`extend T { ... }` 用来补充方法；接口方法用同一种写法声明。
13. **`select` 的分支和 `match` 一样用 `=>`**：`let x = <-ch => ...`、`<-done => ...`、`ch <- v => ...`、`_ => ...`，取代 Go 的 `case ...:` / `default:`；分支体里的 `break` 作用于外层循环。
14. **`v[i]` 绑定**：`let x = v[i]` 对非 copy 元素得到引用（文档语义），想要拥有值就写 `.clone()`。
15. release 构建默认保留溢出检查。
16. **没有 `switch`**：`match` 支持字面量、区间、`|` 和守卫，而且是穷尽检查的。
17. **不引入 `View[Owner, T]`**：结构体永远不存引用，解析器存拥有的数据和 `Span` 区间，用方法借出视图。
18. **`defer` 不能改变函数的去向**：里面不能写 `return`、`try`，也不能跳出外层循环（来自 Zig）。

## 尚未实现

- Windows；语言服务器（LSP）；包管理器（目前一个程序就是它用到的目录）。调试器能按 Kite 的行设断点、显示调用栈，但变量名是生成的 C 里的名字（`p_x`、`l0_total`）。
- 网络只有 TCP 和明文 HTTP/1.1（每个连接一个请求）；没有 UDP、Unix socket、TLS。JSON 还不能和结构体自动互转。
- Linux 6.13 之前的内核上，每个 goroutine 占两个内存映射，默认的 `vm.max_map_count`（262144）约支持 13 万个同时存在的 goroutine，超过时报错并提示调大。
- `#specialize`、PGO。
- 设计文档里的 `RwLock[T]`、`Atomic[T]`、`saturatingAdd`、`unsafe.cast`、`unsafe.forget`，以及把 Kite 函数作为回调传给 C（`extern "C" fn` 目前只能导出和直接调用）。
- 按包分开编译、导出数据缓存（目前每次整体编译，靠前端速度和二进制缓存弥补）。
- 接口值和函数值不满足 `send`（偏保守）；传给 `go` 的闭包按捕获内容单独检查。

## 测试

```sh
cargo test                  # 运行程序并比对输出、编译错误用例、运行时 panic 用例
cargo test -- --ignored     # 在 ASan + UBSan 下运行全部程序，在 TSan 下运行并发程序
```

- `tests/run/*.kite`：程序与期望输出 `.out`
- `tests/errors/*.kite`：必须编译失败，`// error: ...` 注释列出期望的报错
- `tests/panic/*.kite`：必须以退出码 2 panic，并报出调用处位置
- `tests/projects/*/`、`examples/*/`：多包项目，比对 `expected.out` 或 `expected.err`
- `tests/fuzz/*.kite`：模糊测试找到过的编译器崩溃（已化简），必须得到诊断信息或编译成功
- `tests/unsafe/*.kite`：151 个专门构造的绕过安全检查的程序，必须全部被拒绝

### 模糊测试

```sh
cargo build --profile fuzz              # 优化构建，保留调试断言和溢出检查
target/fuzz/kite-fuzz --jobs 4          # 变异模糊测试：编译器崩溃、超时、生成的 C 编不过、ASan 报错
target/fuzz/kite-fuzz diff --jobs 2     # 差分测试：随机生成的程序在 debug / release / sanitizer 下结果必须一致
target/fuzz/kite-fuzz borrow --jobs 2   # 借用检查器：随机摆弄视图的程序，被接受的必须在 ASan 下干净
target/fuzz/kite-fuzz fmt 6000         # 格式化器：保持词法单元不变、结果稳定、与原来的缩进无关
target/fuzz/kite-fuzz reduce fuzz-out/findings/xxx.kite   # 化简复现
```

结果在 `fuzz-out/findings/`。规则和限制见 [`docs/spec.md`](docs/spec.md) 第 15 节。

## 发布

`scripts/release.sh` 在 Mac 上构建四个发布包（`macos-arm64`、`macos-x86_64`、`linux-x86_64`、`linux-arm64`，Linux 版用 `cargo zigbuild` 静态链接 musl）、`SHA256SUMS`、`VERSION` 和 `install.sh`，放在 `dist/`。推送 `vX.Y.Z` 标签时 `.github/workflows/release.yml` 做同样的事并创建 GitHub release（带 `-` 的版本号标为 prerelease）；`ci.yml` 在 Linux（x86-64、arm64）和 macOS 上运行测试，Linux 上还跑 sanitizer。
