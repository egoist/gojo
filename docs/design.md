# Kite 语言设计文档

Nov 4, 2026 · @xinyu zeng

## 1. 概述

Kite（代号，可改）是一门保留 Go 的简单和编译速度、去掉 GC 与 nil 的系统语言。内存靠值语义加二等引用管理：编译期排除空指针和数据竞争，却不需要生命周期标注。

**设计目标**

- **没有 GC**：内存和资源（文件、锁、连接）离开作用域就确定释放，没有停顿，可以嵌入其他程序、写底层代码。
- **没有 nil**：可能为空的值必须写成 `?T`，用之前必须解开。
- **编译期没有数据竞争**：跨 goroutine 共享可变状态只能经过 `Mutex[T]` 这类类型。
- **编译速度保持 Go 的量级**：检查只看函数签名和单个函数体，从不跨函数推断。
- **学习曲线接近 Go**：不写生命周期，不写 trait 实现，没有宏。

**非目标**

- 不追求 Rust 的全部表达力。把引用存进结构体的写法（零拷贝解析器之类）交给 `unsafe` 或专门的视图类型。
- 不兼容 Go 源码，也不能直接调用 Go 代码：Go 的 GC 堆和 Kite 的所有权模型无法安全混用。
- 不做宏、编译期求值、高阶类型、特化。

**从 Go 原样保留**：以包为编译单元、导入无环、结构化的隐式接口、goroutine、channel 和 `select` 的语义、组合而非继承、struct 嵌入、gofmt 式的唯一格式、单一静态二进制、内置的测试和工具链。

## 2. 词法与基础语法

语法以 Go 为起点，但不必照搬。类型写在名字后面、大括号不换行、没有分号这些保留下来。原则是：写法和 Go 相同，含义也必须相同；含义变了，就换一种写法，不让人踩“看着一样、行为相反”的坑。

```go
use os

struct Config {
    name  String            // 必填：没有默认值
    port  int = 8080        // 字段可以写默认值
    proxy ?String           // 可选，默认 none
}

fn load(path str) Config ! os.Error {
    let data = try os.readFile(path)   // data 是 Vec[byte]，离开函数时释放
    return parse(data)
}

fn main() {
    let cfg = load("app.toml") catch e {
        eprintln("using defaults:", e)
        Config{name: "app"}         // catch 块最后一个表达式是它的值
    }
    var hits = 0                    // var 才可变
    for i in 0..10 { hits += i }
    println(cfg.name, cfg.port, hits)
}
```

| 写法 | 含义 | 和 Go 的区别 |
| --- | --- | --- |
| `let x = e` | 不可变绑定 | Kite 没有 `:=`，避免同一写法在两门语言里含义相反 |
| `var x = e`、`var x T` | 可变绑定；`var x T` 要求 `T` 有默认值 | 不是所有类型都有零值 |
| 没有 `package` 声明 | 一个目录就是一个包，包名是目录名 | 不再需要和目录名保持一致的冗余声明 |
| `use fmt, os`、`use ./geom` | 导入标准库模块 / 本项目的包 | 路径不加引号；本地包以 `./`、`../` 开头，和标准库一眼区分 |
| `struct P { fn f(self) {} }` | 方法写在类型里，接收者是 `self` | 取代 Go 的 `func (p *P) f()`；没有值 / 指针接收者之分 |
| `fn name(x int) int` | 函数；函数类型写作 `fn(int) int` | Go 写 `func` |
| `pub fn`、`pub struct` | 导出 | 取代首字母大写，枚举分支和常量的命名不再受限 |
| `for x in xs`、`for i in 0..n` | 遍历集合、区间 | 取代 `range`；`for cond {}`、`for {}` 保留 |
| `(x) => x * 2` | 短闭包，参数类型从上下文推断 | Go 只有 `func(x int) int { ... }` |
| `defer stmt` | 在**当前块**结束时执行 | Go 在函数结束时执行 |
| `if let v = opt { }` | 解开 `?T`，`v` 只在块内有效 | 取代 `if v != nil` |
| `a ?? b` | `a` 为 none 时取 `b` | 新增 |
| `match x { ... }` | 穷尽的模式匹配 | 取代大部分 `switch` 和类型断言 |
| `a +% b`、`a *% b` | 显式的回绕运算 | Go 的 `+` 静默回绕 |

### 2.1 包与导入

一个目录就是一个包，包名就是目录名，文件里不写包声明。`kite run`/`kite build` 的入口包里放 `main` 函数（不需要 `pub`）。导入写在文件开头：

```go
use fmt, os, time              // 标准库：直接写名字，一行可以写多个
use ./geom                     // 本项目的包：相对当前包所在目录的路径
use ../shared/util as u        // 嵌套路径；as 换一个绑定名
use ./geom.{Vec2, Shape}       // 把名字直接引入作用域，此时不绑定 geom
```

- 默认绑定名是路径的最后一段；最后一段不是合法标识符时必须写 `as`。
- `.{...}` 引入的名字必须是 `pub` 的，不能和本包的声明重名。
- 未使用的导入，包括 `.{...}` 里没用到的名字，是编译错误。导入不能成环。
- `print`、`println`、`eprint`、`eprintln` 在 prelude 里，不需要导入；`fmt` 提供 `sprint` 等格式化函数。

### 2.2 声明与语句

类型声明以它的种类开头：`struct Config {}`、`enum Shape {}`、`interface Reader {}`，前面可以加 `pub`、`copy`。`type` 只用于别名 `type Id = u64` 和新类型 `type Celsius f64`，含义与 Go 相同：新类型和底层类型不兼容，要显式转换；底层是数字时保留算术和比较运算符，两边必须是同一个新类型。

- **同一函数内禁止遮蔽**外层变量，根除 `:=` 遮蔽 `err` 的问题。
- 没有 `switch`：对常量分支同样用 `match`（字面量、区间、`|` 组合模式），而且它是穷尽检查的。
- 未使用的导入和变量仍然是编译错误，和 Go 一样。

## 3. 类型系统

每种类型都明确自己是否拥有数据、能否为空、有没有默认值。一个类型并不天然有零值。

| 类型 | 说明 | 有默认值 |
| --- | --- | --- |
| `bool`、`int`、`i8`…`i64`、`uint`、`u8`…`u64`、`f32`、`f64`、`byte`、`rune` | 标量，可按位复制 | 是 |
| `String` / `str` | 拥有的 UTF-8 字符串 / 借用的字符串视图 | `String` 是（空串） |
| `Vec[T]` / `[]T` | 拥有的动态数组 / 借用的切片视图 | `Vec` 是 |
| `[N]T` | 定长数组，放在栈上或内联在结构体里 | `T` 有则有 |
| `Map[K, V]`、`Set[T]` | 内置哈希表 | 是 |
| `?T` | 可选值，`none` 或一个 `T` | 是（`none`） |
| `T ! E` | 结果：成功时是 `T`，失败时是 `E` | 否 |
| `Box[T]` | 堆上独占所有权 | 否 |
| `rc[T]`、`arc[T]`、`weak[T]` | 共享所有权（单线程 / 原子 / 弱引用） | 否 |
| `chan T` | channel，发送即转移所有权 | 否 |
| `fn(A) R` | 函数和闭包，永远不为空 | 否 |
| `*T` | 裸指针，可为空，只能在 `unsafe` 里解引用 | 否 |

**结构体**的字段可以写默认值。字面量里漏写一个没有默认值的字段，就是编译错误。结构体的所有字段都有默认值时，它本身才满足 `default`，可以写 `var c Config`。

**枚举**是真正的和类型，分支可以携带数据；不带数据时就是普通枚举，取代 `iota`。

```go
enum Shape {
    Circle(r f64)
    Rect(w f64, h f64)
}

enum Level { Debug, Info, Warn, Error }

fn area(s Shape) f64 {
    return match s {
        Circle(r)  => math.Pi * r * r
        Rect(w, h) => w * h
    }                       // 漏掉任何一个分支都是编译错误
}

log(.Warn, "disk almost full")   // 类型已知时可以省略枚举名
```

**可选值 `?T`** 的操作：`if let v = x { }` 解开，`x ?? d` 取默认值，`x?.field` 链式访问（结果仍是可选值），`x.unwrap()` 在 none 时 panic。`?T` 的内存布局会利用非空类型的空位：`?Box[T]` 和一个指针一样大。

**整数溢出**：debug 构建中 `+ - *` 溢出就 panic，release 构建中默认也检查，可以按包关闭。需要回绕时显式写 `+%`，需要饱和运算时调用 `a.saturatingAdd(b)`。数组和切片的下标始终做边界检查。

## 4. 所有权与借用

每个值恰好有一个所有者，所有者离开作用域时值被释放。引用是**二等公民**：只能作为参数传入或从投影借出，不能存进结构体、集合或会逃逸的闭包。因此所有检查都在单个函数内完成，不需要生命周期标注。

### 4.1 move 与 copy

- 赋值、传参给 `sink`、返回、发送到 channel，都会**转移**所有权。之后再用原变量就是编译错误。
- 标量和 `copy` 类型按位复制：`copy struct Point { x, y f64 }`。含有需要释放的字段（`String`、`Vec`、`Box`……）或有 `drop` 方法的类型不能是 `copy`。
- 导出类型必须显式声明 `copy`；包内私有的非泛型类型，所有字段都是 copy 且没有 `drop` 方法时自动成为 `copy`。这样给公开类型加字段永远不会悄悄改变下游代码的语义。
- 其他类型要复制，就显式调用 `.clone()`，开销在代码里看得见。

### 4.2 参数模式

| 模式 | 签名 | 调用处 | 含义 |
| --- | --- | --- | --- |
| 只读借用（默认） | `fn show(u User)` | `show(u)` | 不能修改，不能逃逸；`copy` 类型可能直接传值 |
| 可变借用 | `fn rename(u inout User)` | `rename(&u)` | 独占：同一时刻只有一个可变借用 |
| 转移所有权 | `fn store(u sink User)` | `store(u)` | 调用后 `u` 不能再用 |

方法的接收者写成第一个参数 `self`，同样分三种：`fn show(self)`、`fn rename(self inout)`、`fn store(self sink)`（见 6.1）。调用处的 `&` 只用于 `inout` 参数，读代码时一眼就能看出哪些实参会被改；接收者不需要写 `&`。

**独占性规则**：同一个值在同一时刻，要么有任意多个只读借用，要么只有一个可变借用。`swap(&a, &a)` 和 `f(a, &a)` 都是编译错误。

### 4.3 投影：借出引用

容器必须能把内部元素借出去，否则每次访问都得复制。投影函数用 `yield` 借出引用。借出的引用（以及 `str`、`[]T` 这类视图）在当前语句内有效；绑定到 `let` 时，它在块内**最后一次使用之后**就不再占用被借的值，所以借用结束后可以继续修改原值。

```go
extend Vec[T] {
    pub fn at(self, i int) yield &T
    pub fn atMut(self inout, i int) yield inout T
}
extend Map[K, V] {
    pub fn get(self, k K) yield ?&V
    pub fn entry(self inout, k sink K) yield inout V where V: default
}

let x = v[i]                 // v[i] 是 v.at(i) 的语法糖；T 是 copy 时复制出来
v[i] += 1                 // 可变投影，只在这条语句内有效
if let u = users.get(id) {  // u 是借用，只在块内有效；块内不能修改 users
    greet(u)
}
with e = counts.entry(word) {   // with 把投影绑定到一个块
    e += 1
}
```

**视图只存在于变量里。** 没有生命周期标注，借用检查器只能跟踪放在局部变量里的视图。所以任何会把视图存进别处的值都不能存在：集合、`Box`、`rc`、接口值、泛型类型，不管类型是写出来的还是推断出来的（`[s]`、`rc(s)`）。视图赋给外层变量时，被借的值必须活得至少一样长；锁里的数据的视图不能离开 `with` 块，因为块结束时锁就释放了。泛型函数如果在调用结束后还保留 `T` 的值（逃逸的闭包、goroutine），调用时 `T` 不能是视图。这几条是 M2 阶段系统地构造绕过用例时补上的，见 `tests/errors/view_*`、`stored_views`、`generic_keeps_view`。

### 4.4 释放与 defer

- 定义了 `fn drop(self inout)` 方法的类型在离开作用域时自动调用它。同一作用域内的变量按声明顺序的**逆序**释放，结构体字段按声明顺序释放。
- 被 move 走的变量不再释放。只在部分分支里 move 的变量，编译器插入一个标志位。
- `defer` 在当前块结束时执行。由于资源会自动释放，`defer f.Close()` 这类写法大多不再需要；defer 主要用于恢复状态、打日志。被推迟的代码不能改变函数的去向：里面不能写 `return`、`try`，也不能跳出外层循环。
- 需要提前释放时调用 `drop(x)`，它就是一个接收 `sink` 参数的普通函数。

### 4.5 共享所有权与有环结构

- `rc[T]` 非原子计数，不能跨 goroutine；`arc[T]` 原子计数。两者内部都只读，要修改就包一层 `Cell[T]`（单线程）或 `Mutex[T]`。
- 环由程序员用 `weak[T]` 打破，语言不做环检测。
- 图、AST、DOM 这类结构推荐用 `arena[T]` 加下标：节点之间用 `Id[T]` 互相引用，整个 arena 一次释放。`Id` 带代数，用已删除的 Id 访问会得到 none，而不是野指针。

### 4.6 Vec 与切片视图

`Vec[T]` 拥有数据，`[]T` 只是借用的视图。`v.push(x)` 要求 `inout` 借用，所以存在任何视图时都不能扩容。Go 里“append 之后两个切片意外共享底层数组”的问题因此不会发生。`String` 和 `str` 的关系与此相同。

### 4.7 闭包捕获

- 作为参数传入、不逃逸的闭包（默认模式）可以借用外层变量，例如 `v.each((x) => total += x)`。
- 会逃逸的闭包（`sink fn()`、存进结构体、交给 `go`）必须**拥有**捕获的变量：`copy` 类型自动复制，其他类型被 move 进闭包。需要保留原变量时先 `.clone()`。

## 5. 错误处理

错误仍然是值，但类型系统保证它被处理。`T ! E` 取代 `(T, error)`，`try` 取代 `if err != nil { return err }`。

```go
enum LoadError {
    Io(os.Error)
    Syntax(line int, msg String)
}

fn load(path str) Config ! LoadError {
    let data = try os.readFile(path) else LoadError.Io   // 把 os.Error 包成 LoadError
    return try parse(data)                           // 类型相同，直接传播
}

fn save(c Config) ! os.Error { ... }               // 只可能失败、没有结果值

let cfg = load(p) catch e { Config.default() }          // 就地处理

match load(p) {                                      // 逐个处理
    ok(c)                   => run(c)
    err(.Syntax(line, msg)) => eprintln("line", line, msg)
    err(.Io(e))             => eprintln(e)
}
```

- 调用返回 `T ! E` 的函数却不处理结果，是编译错误。确实要丢弃，就显式写 `_ = f()`。
- `try x` 在错误类型相同、或外层错误类型是 `error` 时直接传播；否则必须写 `try x else 包装函数`。没有隐式转换。
- `error` 是内置接口（只有 `fn message(self) String` 一个方法），用在应用的顶层，相当于 Go 的 `error`。库应该返回具体的枚举错误。
- `x.unwrap()` 在出错时 panic，只用于测试和确实不可能失败的地方。

**panic** 只表示程序 bug，比如越界、溢出、`unwrap` 失败。panic 打印消息和源码位置，然后**终止整个进程**（退出码 2），不展开栈。

- 任何 goroutine、任何 task 里的 panic 都结束进程。没有 `recover`，也没有 `task.Panic`。
- 理由：不生成展开表，二进制更小，正常路径没有任何开销；bug 发生后，内存和锁的状态已经不可信，继续运行同一个进程比重启更危险。需要隔离故障的服务，用进程级的监管（多进程或外部重启）。
- 测试例外：`kite test` 把每个测试放在单独的进程里运行，一个测试 panic 不影响其他测试。

## 6. 接口与方法

接口保持 Go 的结构化、隐式满足：一个类型只要有这些方法就满足接口，不需要声明。变化在于：接口值永远不为空，并且要区分是拥有还是借用。

```go
interface Reader {
    fn read(self inout, buf inout []byte) int ! io.Error
}

fn copyAll(dst inout Writer, src inout Reader) int ! io.Error   // 借用：胖指针，不分配内存

var r Reader = try os.open("a.txt")   // 拥有：File 被 move 进堆上的独占盒子
let readers = Vec[Reader]{}            // 异构集合
let shared = arc[Logger](newLogger())  // 需要共享时显式写出来

match r {                           // 取代类型断言和 type switch
    f File    => println("file", f.name)
    b Buffer  => println("buffer", b.len)
    _         => {}
}
```

| 写法 | 内存表示 | 用途 |
| --- | --- | --- |
| 参数 `r Reader` / `r inout Reader` | 数据指针 + 方法表，不分配 | 绝大多数函数参数 |
| 值 `Reader`（变量、字段、集合元素） | 堆上独占的盒子 + 方法表，释放时 drop 内部值 | 存储异构对象 |
| `rc[Reader]`、`arc[Reader]` | 引用计数的盒子 | 共享 |
| `?Reader` | 同上，空位表示 none | 可选 |

- Go 里“带类型的 nil 不等于 nil”的问题不存在：接口值不可能为空，可空就写 `?Reader`。
- 方法集的计算不再区分值接收者和指针接收者。接口方法和普通方法用同一种写法声明，`self` 的模式（借用 / `inout` / `sink`）实现要和接口一致。
- 带类型参数的方法和条件方法**不计入**接口方法集（见第 7 节），因为动态派发时没法为它们生成代码。
- struct 嵌入和接口嵌入都保留：嵌入的字段归外层 struct 所有，它的方法被提升到外层。

### 6.1 方法的写法

方法写在类型声明的大括号里，接收者是显式的第一个参数 `self`；没有 `self` 的函数是关联函数，通过类型调用。

```go
pub struct Stack[T any] {
    items Vec[T]

    pub fn new() Stack[T] {                  // 关联函数：Stack[String].new()，或由期望类型推断
        return Stack[T]{items: Vec[T]{}}
    }

    pub fn push(self inout, x sink T) {     // 可变接收者
        self.items.push(x)
    }

    pub fn peek(self) yield ?&T {           // 借用接收者；类型参数 T 来自外层类型
        ...
    }

    pub fn into(self sink) Vec[T] {         // 消耗接收者
        return self.items
    }
}

extend []String {                             // 标准库给内置类型补方法，可以针对特定实例
    pub fn join(self, sep str) String { ... }
}
```

- 方法可以写在 `struct` 和 `enum` 的声明里；`extend T { ... }` 用于把方法放到别的文件、给特定实例（如 `[]String`）加方法，以及标准库给内置类型加方法。
- 方法和关联函数只能定义在声明该类型的包里。这样一个类型的方法集只取决于它自己的包，分开编译时下游无需重新检查。
- 字段和方法、方法和关联函数不能重名。
- 相比 Go 的 `func (s *Stack) push(x int)`：方法和类型放在一起；接收者统一叫 `self`，不用每个方法起一个名字；不再有值接收者和指针接收者两套规则，模式和普通参数一样写；构造函数有了归属（`Stack.new()`），不必写成包级的 `newStack()`。

## 7. 泛型

泛型补齐 Go 日常会碰到的缺口：方法的类型参数、条件方法、无 GC 必需的能力约束。类型系统本身不做成计算系统。默认实现是字典传递，字典里除了方法表，还带着 `size`、`align` 和 `drop` 函数指针。

```go
pub struct Vec[T any] {
    ...

    // 方法可以有自己的类型参数（不计入接口方法集）
    pub fn map[U any](self, f fn(T) U) Vec[U]
    pub fn filter(self, f fn(T) bool) Vec[T] where T: clone

    // 条件方法：只在 T 满足约束时存在
    pub fn sort(self inout) where T: ordered
    pub fn clone(self) Vec[T] where T: clone
}

// 约束写在类型参数旁，或写在 where 里
fn fill[T copy](n int, x T) Vec[T]
fn max[T ordered](xs []T) ?T

let names = users.filter((u) => u.active).map((u) => u.name.clone())
```

**能力约束**是内置的，由编译器判断一个类型是否满足：

| 约束 | 含义 | 如何满足 |
| --- | --- | --- |
| `any` | 任意类型，只能 move 和借用 | 所有类型 |
| `copy` | 按位复制，赋值不 move | 标量；声明为 `copy struct` 的类型 |
| `clone` | 可以显式 `.clone()` | 有 `clone()` 方法；`copy` 类型自动满足 |
| `default` | 有默认值，可以写 `var x T` | 所有字段都有默认值 |
| `send` | 可以跨 goroutine 转移 | 编译器按结构推导：不含 `rc`、`Cell`、裸指针 |
| `comparable` | 支持 `==` | 内置类型；有 `equal(other T) bool` 方法 |
| `ordered` | 支持 `<` | 数字和字符串；有 `compare(other T) Ordering` 方法 |
| `hash` | 可以做 `Map` 的键 | `comparable` 且有 `hash(h inout Hasher)` 方法 |

自定义类型实现 `equal` 和 `hash` 就能当 map 的键，这是 Go 做不到的。但 Kite 只对 `==` 和比较开这个口子，没有一般性的运算符重载。

**用户定义的约束**仍然是接口，可以包含类型集 `~int | ~float64`。含类型集的接口只能当约束；需要“几种类型之一”的值时，用 `enum`。

**整数常量泛型**用于定长数组、矩阵和 SIMD。只允许整数参数，类型层面不做运算。数组大小决定内存布局，所以这部分按 N 单态化。

```go
struct Mat[R int, C int] { data [R][C]f32 }
extend Mat[R, K] {
    fn mul[C int](self, b Mat[K, C]) Mat[R, C]   // 维度不匹配就是编译错误
}
```

**迭代器**。持有借用的迭代器对象违反“引用不能存起来”的规则，所以 Kite 的迭代协议有两种，都不需要存引用：

- **游标（pull）**：`fn cursor(self) C` 返回一个普通数据的游标，`fn next(self, c inout C) yield ?&T` 每次借出一个元素（`nextMut` 用于 `for x in &t`；`next` 也可以直接返回拥有的 `?T`，用作生成器）。`for` 被翻译成“取游标 + 循环调用 `next`”，循环持有对 `t` 的借用，`break`、`continue`、`return`、`try` 全部照常工作，没有任何开销。`zip`、归并只是同时推进两个游标，不需要协程。
- **推送（push）**：`fn each(self, body fn(T) Flow)`（`eachMut` 用于 `&t`），适合递归结构（树的中序遍历）。循环体是不逃逸的闭包，`break`、`continue`、`return`、`try` 以及带标签的跳转由编译器改写成控制信号。循环体可能执行多次，所以不能 move 外层变量。离开循环后循环体不会再被执行，哪怕 `each` 忽略了返回的 `Flow`。

两种都提供时用游标协议。

```go
extend Tree[T] {
    pub fn each(self, body fn(&T) Flow)
}

for k, v in m { }            // Map 的 each 给出两个借用
for x in &v { x += 1 }      // 可变遍历：调用 eachMut
```

循环可以带标签：`outer: for row in grid { for x in row { if x == 0 { continue outer } } }`。

## 8. 并发

goroutine、channel 和 `select` 保留，调度器仍是 M:N 的有栈协程，本来就不依赖 GC。所有权让数据竞争变成编译错误；结构化并发让子任务可以安全地借用外层数据。

栈和 Go 不同：任务会借用别的 goroutine 栈上的数据，所以栈**不能移动**，也就不能像 Go 那样从 2 KB 开始、满了再整体复制。每个 goroutine 预留一段固定的虚拟地址（默认 2 MB，带保护页），只有用到的页才占内存：一个挂起的小 goroutine 占一页（Linux 4 KB，Apple Silicon 16 KB）。阻塞的 C 调用不需要特殊标注：`extern` 调用期间线程把 CPU 名额让给别的线程。不阻塞的长计算会被抢占：生成的代码在循环和递归函数入口检查一个全局标志（一次读、一次分支），有 goroutine 在等时，运行超过 10 ms 的 goroutine 让出 CPU。8 个基准的周期数几何平均没有变化，单项最多 +5%（`bench/README.md`）。

```go
// 结构化并发：块结束时等待所有子任务；任一失败就取消其余任务
fn fetchAll(urls []String) Vec[Page] ! http.Error {
    var pages = Vec[Page]{}
    let mu = Mutex(&pages)              // 锁只在这个作用域内借用 pages
    try task.group((g) => {
        for u in urls {
            g.go(() => {                // 可以借用 u 和 mu：group 保证它们活得更久
                let p = try http.get(u)
                with list = mu.lock() { list.push(p) }
            })
        }
    })
    return pages
}

// channel 发送即 move
let ch = chan[Job](16)
go worker(ch.clone())                   // 裸 go：参数和捕获都必须是拥有的、send 的
ch <- job                               // job 被 move 走，发送方不能再碰
if let j = <-ch { run(j) }              // 接收返回 ?Job，channel 关闭时是 none

// 长期共享的状态
let stats = arc(Mutex(Stats{}))
with s = stats.lock() { s.hits += 1 }   // 锁在块结束时释放，引用带不出块
```

- **跨 goroutine 的值必须满足 `send`**：编译器按类型结构自动推导，用户不写。含 `rc`、`Cell` 的值传给 `go` 或发送到 channel，就是编译错误。
- **共享可变状态只能靠 `Mutex[T]`、`RwLock[T]`、`Atomic[T]`**：数据归锁所有，不加锁就拿不到。加锁是一个投影，受保护的引用不可能带出临界区。
- **`task.group` 是首选**：子任务可以借用组外的数据，组体里声明的值则被子任务拿走（和 `go` 一样）；第一个错误会取消其余子任务，然后作为 group 的错误返回（panic 则终止进程，见第 5 节）。子任务也可以用 `g.cancel()` 取消整个组。
- **取消沿任务树传播**，不需要在每个函数签名里传 `ctx`。被取消的任务里，阻塞操作立即返回：接收得到 `none`，发送不发生，`time.sleep` 提前醒来，网络调用返回 `isCancelled()` 的错误。不阻塞的长计算用 `try task.check()` 设检查点。超时写成 `task.timeout(5 * time.second, () => ...)`，期限到了结果是 `none`。
- 取消不会让结果“半途消失”：组仍然等每个子任务结束，所以子任务借用的数据始终有效。
- **裸 `go f(x)` 仍然保留**，用于确实要和调用方脱离的后台任务。它的参数和捕获必须被 move 进去，并且满足 `send`；它不属于任何组，不会被取消。
- `select` 保留 Go 的语义，分支写法和 `match` 一致：

```go
select {
    let job = <-jobs => run(job)      // 接收并绑定：job 是 ?Job，通道关闭时为 none
    <-done           => return        // 只接收，不绑定
    results <- r     => sent += 1     // 发送
    _                => idle()        // 都没有就绪时立即执行；没有 `_` 就等待
}
```

  分支体里的 `break` / `continue` 作用于外层循环（`select` 本身不是循环）。

## 9. unsafe 与 FFI

安全代码里没有空指针、野指针和数据竞争；做不到这一点的操作都必须写在 `unsafe` 块里。标准库中 `Vec`、`Map`、`arc` 的实现用的就是它。

```go
extern "C" lib "libz" {                       // 运行时 dlopen，构建时不需要 C 工具链
    fn crc32(crc u64, buf *u8, len u32) u64
}

pub fn checksum(data []byte) u64 {
    return unsafe { crc32(0, data.ptr(), u32(data.len)) }
}

struct Node {
    next *Node                              // 裸指针可以存进结构体，可以为空
}
```

| 只能在 `unsafe` 里做 | 说明 |
| --- | --- |
| 解引用 `*T` | 裸指针可以创建和传递，只有解引用不安全 |
| 指针运算、`unsafe.cast` | 任意类型重解释 |
| 调用 `extern` 函数 | 编译器无法验证对方是否遵守所有权 |
| 手动声明类型满足 `send` | `unsafe send struct Pool { ... }`，由作者担保 |
| `unsafe.forget(x)` | 放弃释放，把所有权交给外部代码 |

- `unsafe` 块的范围尽量小。编译器会统计每个包的 unsafe 行数，并写进包的元数据，供审查和依赖策略使用。
- `extern "C" lib "z"` 的库在运行时加载，和不依赖 cgo 的做法一样，交叉编译不需要目标平台的 C 工具链。libc 的函数（不写库）和 `extern "C" static "z"` 是普通的链接期调用。
- 外部回调由 `extern "C" fn` 声明的 Kite 函数提供，不能捕获变量，上下文通过 `*void` 参数传递。
- panic 不能穿过 `extern` 边界：在回调中发生 panic 时进程直接中止。

## 10. 编译模型与性能

目标是 debug 构建不慢于 Go 的 1.3 倍。这是估计值，待原型实测。前提是守住两条线：所有检查只看函数签名和单个函数体；泛型默认用字典传递，不单态化。

**编译单元是包**，导入不能有环。下游包只读上游的导出数据：签名、参数模式、类型布局、能力约束（`copy`、`send`……）的推导结果。修改函数体不会触发下游重新检查。

| 阶段 | 工作 | 成本 |
| --- | --- | --- |
| 解析与类型检查 | 和 Go 相同，只做局部的双向类型推断 | 与 Go 相当 |
| 借用检查 | 对每个函数体做一遍数据流分析，函数之间并行 | 低，接近线性 |
| 释放代码插入 | 在作用域出口和条件 move 处插入 drop 和标志位 | 低，但中间代码会变多 |
| 逃逸分析 | **不再需要**：值在栈上还是堆上由类型决定 | 省掉了 Go 的这部分 |
| 泛型 | debug：全部字典传递；release：对热点和小函数单态化 | debug 低，release 中 |
| 后端 | debug 用自带的快速后端；release 可选做更多内联 | debug 与 Go 相当 |

**release 构建的单态化**只针对三类函数：PGO 数据标出的热点、小于内联阈值的小函数、标了 `#specialize` 的函数。整数常量泛型始终按 N 实例化。

会把编译速度拖向 Rust 的做法，都被排除在外：从函数体推断参数模式、过程宏、编译期求值、可以重叠的实现、默认单态化。

## 11. 与 Go 的差异速查

| 主题 | Go | Kite |
| --- | --- | --- |
| 内存管理 | GC + 逃逸分析 | 所有权 + 确定释放，共享时用 `rc` / `arc` |
| 空值 | 指针、接口、map、chan、函数都可能是 nil | 只有 `?T` 可以为空 |
| 绑定 | `x := e`、`var x = e`，全部可变 | `let x = e` 不可变，`var x = e` 可变 |
| 类型声明 | `type Config struct {}` | `struct Config {}`、`enum`、`interface` |
| 零值 | 每种类型都有 | 只有满足 `default` 的类型有 |
| 包声明 | 每个文件写 `package name`，须与目录一致 | 不写；目录即包 |
| 导入 | `import "fmt"`、`import "example.com/m/geom"` | `use fmt`、`use ./geom`、`use ./geom.{Vec2}` |
| 函数 | `func f(x int) int` | `fn f(x int) int` |
| 方法 | `func (p *Point) scale(k int)`，值 / 指针接收者 | 写在类型里：`fn scale(self inout, k int)`；`extend T {}` 补充 |
| 构造函数 | 包级函数 `NewStack()` | 关联函数 `Stack.new()` |
| 导出 | 首字母大写 | `pub` |
| 枚举 | `const` + `iota` | `enum` 和类型 + 穷尽 `match` |
| 错误 | `(T, error)` + `if err != nil` | `T ! E` + `try` / `catch` |
| panic | 可以 `recover` | 不能 recover，终止进程 |
| 类型判断 | 类型断言、type switch | `match` |
| 切片 | 可能与其他切片共享底层数组 | `Vec[T]` 拥有，`[]T` 借用，不会意外共享 |
| 参数 | 传值或传指针 | 只读借用 / `inout` / `sink` |
| defer | 函数结束时执行 | 块结束时执行 |
| 遍历 | `for i, x := range xs` | `for x in xs` |
| 闭包 | `func(x int) int { ... }` | 另有 `(x) => expr` |
| `select` | `case v := <-ch:`、`default:` | `let v = <-ch => ...`、`_ => ...`，与 `match` 同一写法 |
| 整数溢出 | 静默回绕 | panic；回绕写 `+%` |
| 数据竞争 | 运行时 race detector | 编译期排除 |
| 取消 | 手动传 `context.Context` | 沿任务树自动传播 |
| 泛型方法 | 不支持 | 支持，不计入接口方法集 |
| map 的键 | 只能是内置可比较类型 | 任何满足 `hash` 的类型 |
| C 互操作 | cgo | `extern`，运行时加载 |

## 12. 刻意不做的特性与已定决议

**刻意不做**

| 特性 | 不做的原因 |
| --- | --- |
| 存储引用与生命周期标注 | 这是 Rust 难学的主要来源；用投影和 arena 覆盖绝大多数场景 |
| 类与继承 | 组合和结构化接口已经够用 |
| trait 式显式实现 | 会引入孤儿规则和重叠问题，破坏分开编译 |
| 高阶类型、特化、变长泛型 | 类型检查会变成求解问题 |
| 宏和编译期求值 | 编译变慢、工具难以理解代码；代码生成交给外部工具 |
| 一般性运算符重载 | 只开放 `==`、比较和哈希 |
| 异常和 `recover` | 错误走 `T ! E`，panic 只表示 bug |
| async / await | goroutine 没有“函数染色”问题 |
| 全局类型推断 | 报错信息难懂，编译变慢 |

**已经定下的问题**（原第 12 节的开放问题）

- [x] **copy 是否自动推导**：采用折中方案。导出类型必须显式写 `copy`；包内私有的非泛型类型在所有字段都是 copy、且没有 `drop` 方法时自动推导为 `copy`。
- [x] **pull 迭代器**：不用协程。游标协议（`cursor` + `next`）提供零开销的 pull 迭代，`zip` 和归并就是同时推进几个游标；递归结构用推送协议 `each`（见第 7 节）。
- [x] **自引用和零拷贝视图**：不引入 `View[Owner, T]`。解析器在结构体里存拥有数据的 `String` / `Vec` 和整数区间 `Span`，需要时用方法借出视图（`span.of(self.src)`）。结构体里永远不存引用，规则保持简单，也不需要 `unsafe`。
- [x] **panic 展开的开销**：默认也是唯一的模式是 abort，见第 5 节。
- [x] **整数溢出在 release 中的默认行为**：默认检查。实测（`bench/`）把检查分支所在的小函数内联之后，大部分基准与不检查持平；递归调用密集的代码约慢 1.6 倍，热点处用 `+%` 显式回绕。按包关闭留到有包清单之后。
- [x] **取消怎样表现**：阻塞操作提前返回它本来就有的“没有结果”形式（接收是 `none`，网络是错误，`timeout` 是 `none`），而不是统一返回 `task.Cancelled`。否则每次接收、每次 `sleep` 都要变成结果类型，处处写 `try`。需要显式检查点时用 `try task.check()`。
- [x] **goroutine 的栈**：固定地址、按需占页，不移动，因为任务可以借用别的栈上的数据。代价是每个挂起的 goroutine 至少占一页内存。
- [x] **与 Go 的互操作**：不属于语言。需要时通过 C ABI 和 `extern` 调用；生成声明的工具留给生态。
- [x] **名字**：沿用 Kite。
