# Kite 语言参考

这份文档描述当前编译器实现的语言，是 [design.md](design.md) 的精确版本：设计文档讲为什么，这里讲规则本身。每一节末尾的“测试”列出覆盖这些规则的测试文件：

- `run/`：可运行的程序，输出必须与同名 `.out` 一致；
- `errors/`：必须编译失败，并给出文件里 `// error:` 后面写的信息；
- `panic/`：运行时必须以退出码 2 终止，并给出 `// panic:` 后面写的信息；
- `projects/`：多包项目；`testblocks/`：`kite test`。

测试都在 `tests/` 目录下，由 `cargo test` 运行；`cargo test -- --include-ignored` 还会在 AddressSanitizer / UBSan 下重跑所有 `run/` 程序。

## 1. 词法

- 源文件是 UTF-8。注释有 `// 行注释` 和 `/* 块注释 */`，块注释可以跨行。
- **自动分号**：行尾的 token 是标识符、字面量、`true`、`false`、`none`、`)`、`]`、`}`、`return`、`break`、`continue` 时插入语句结束符，所以以运算符结尾的行会延续到下一行。圆括号和方括号里的换行不算语句结束。`;` 可以把多条语句写在一行。
- 关键字：`use extend pub fn struct enum interface type copy let var if else for in match return break continue defer errdefer go try catch with yield unsafe extern none true false inout sink where test const chan select`。`self`、`as`、`lib` 是上下文关键字。
- 整数字面量：十进制、`0x`、`0b`、`0o`，可以用 `_` 分隔。浮点：`1.5`、`2.5e3`、`1e-3`。
- 字符串 `"..."` 支持 `\n \t \\ \" \xNN \u{...}`；反引号字符串是原样的，不处理转义。字符字面量 `'é'` 是 `rune`（`i32` 码点），期望类型是 `u8` 时转换为字节。
- 无类型的整数字面量默认为 `int`，浮点字面量默认为 `f64`；有期望类型时采用期望类型，超出范围是编译错误。

测试：`run/lexical`、`run/patterns`、`errors/int_mismatch`、`errors/literals`。

## 2. 包与导入

- 一个目录是一个包，包名是目录名，文件里不写包声明。入口包里的 `main` 函数是程序入口，不需要 `pub`。
- `use fmt, os` 导入标准库模块；`use ./geom`、`use ../util/text` 导入本项目的包，路径相对当前包所在的目录。
- 默认绑定名是路径最后一段；`use ./util/text as t` 换一个名字。
- `use ./geom.{Vec2, Shape}` 把名字直接引入作用域，此时不绑定 `geom`。引入的名字必须是 `pub` 的、必须存在。
- 未使用的导入（包括 `.{...}` 里未用到的名字）是编译错误。包之间的导入不能成环。
- 只有 `pub` 的声明能被其他包使用。函数、类型、方法、关联函数和结构体字段默认都对包外不可见。
- `print`、`println`、`eprint`、`eprintln`、`assert`、`assertEq`、`drop`、`ok`、`err`、`some`、`none` 在 prelude 里，不需要导入。

测试：`projects/app`、`projects/cycle`、`projects/private`、`errors/bad_use`、`errors/bad_use_names`、`errors/unused`。

## 3. 声明

### 3.1 变量与常量

- `let x = e` 不可变；`var x = e` 可变；`var x T` 要求 `T` 满足 `default`（第 4.6 节），值是默认值。
- 给 `let` 绑定赋值是编译错误。
- **同一函数内禁止遮蔽**：内层块不能声明和外层局部变量同名的变量。
- 声明了却从未使用的局部变量是编译错误；以 `_` 开头的名字和 `_` 本身不受此限制。
- `const Name = 常量表达式`，可以引用其他常量，整数常量可以用作数组长度。

测试：`run/basics`、`run/decls`、`errors/let_immutable`、`errors/shadowing`、`errors/unused`、`errors/constraints`。

### 3.2 类型声明

- `struct`、`enum`、`interface` 以种类开头，前面可以加 `pub`、`copy`。
- `type A = B` 是别名，与 `B` 完全相同。`type Celsius f64` 是新类型：与底层类型不兼容，用 `Celsius(x)` 和 `f64(c)` 显式转换。
- 底层是数字的新类型保留数字的运算符（`+ - * / %`、`+% -% *%`、整数的位运算、一元 `-`、比较），两边必须是同一个新类型，结果也是这个新类型。新类型在底层类型满足 `ordered`、`default`、`hash` 时也满足它们。其他新类型（如 `type Name String`）只有比较，没有运算符。
- 结构体嵌入：字段写成一个类型名（`struct Admin { Named }`），被嵌入类型的字段和方法提升到外层。
- 递归类型必须经过间接层（`Box`、`Vec`、`rc` 等），否则大小无限，是编译错误。

测试：`run/decls`、`run/constraints`、`errors/infinite_type`、`errors/constraints`。

### 3.3 函数与方法

- `fn name(a A, b B) R`；返回类型省略时为 unit。
- 方法写在 `struct`/`enum` 的大括号里，第一个参数是 `self`，可以带模式：`self`、`self inout`、`self sink`（第 6 节）。没有 `self` 的是关联函数，通过类型调用：`Point.new()`、`Stack[String].new()`；类型参数可以由期望类型推断。
- `extend T { ... }` 给类型补充方法，可以针对特定实例（`extend []String`）。方法只能定义在声明该类型的包里。
- 字段与方法、方法与关联函数不能重名。`self` 只能出现在方法里。
- 接收者不写 `&`：`v.push(x)` 对 `self inout` 方法自动取可变借用。

测试：`run/basics`、`run/generics`、`run/interfaces`、`errors/methods`、`errors/assoc_calls`、`errors/self_outside_method`、`errors/dup_members`。

## 4. 类型

### 4.1 内置类型

| 类型 | 说明 |
| --- | --- |
| `bool`、`int`/`i8`…`i64`、`uint`/`u8`…`u64`、`f32`、`f64`、`byte`（= `u8`）、`rune`（= `i32`） | 标量，copy |
| `String` / `str` | 拥有的 UTF-8 字符串 / 借用视图 |
| `Vec[T]` / `[]T` | 拥有的动态数组 / 借用的切片视图 |
| `[N]T` | 定长数组；`N` 是整数常量或整数类型参数 |
| `Map[K, V]` | 哈希表，迭代顺序是插入顺序 |
| `?T` | `none` 或一个 `T` |
| `T ! E`、`! E` | 成功值 `T` 或错误 `E`；`! E` 没有成功值 |
| `Box[T]` | 堆上独占所有权 |
| `rc[T]`、`arc[T]`、`weak[T]` | 非原子 / 原子引用计数，弱引用 |
| `Cell[T]`、`Mutex[T]`、`MutexRef[T]` | 单线程内部可变性；锁；借用外部数据的锁（第 9 节） |
| `chan T` | channel |
| `fn(A) R` | 函数和闭包，不为空 |
| `*T` | 裸指针，可以为空 |
| `Span` | `copy struct { lo int, hi int }`，字符串里的区间 |

- 没有 `nil`。只有 `?T` 可以为空。
- 不同的整数类型之间、整数和浮点之间没有隐式转换，`i64(x)`、`f64(n)` 显式转换；窄化转换在值超出范围时 panic。
- `?Box[T]`、`?rc[T]`、`?arc[T]`、`?weak[T]` 与一个指针一样大，`none` 是空指针。

测试：`run/basics`、`run/niche`、`run/spans`、`errors/no_nil`、`errors/int_mismatch`、`panic/conversion`。

### 4.2 结构体

- 字段可以写默认值：`port int = 8080`；`?T` 字段默认是 `none`。字面量里漏掉没有默认值的字段是编译错误。
- `copy struct` 声明按位复制的类型；字段必须都是 copy，类型不能有 `drop` 方法。
- **copy 推导**：包内私有、非泛型、所有字段都是 copy 且没有 `drop` 方法的结构体和枚举自动成为 copy。`pub` 类型必须显式写 `copy`。
- 字段都可比较时自动获得 `==`；字段都可哈希时可以做 `Map` 的键；字段都可 clone 时自动获得 `.clone()`。定义 `equal`、`hash`、`compare`、`clone` 方法可以替换自动生成的实现；只定义 `equal` 不定义 `hash` 的类型不能做键。
- 结构体字段不能是视图类型（`str`、`[]T`、`MutexRef`）。

测试：`run/basics`、`run/copyinfer`、`run/interfaces`、`errors/bad_copy`、`errors/copy_exported`、`errors/missing_field`、`errors/view_field`。

### 4.3 枚举

- 分支可以带命名字段：`Circle(r f64)`，也可以不带：`enum Level { Debug, Info }`。
- 期望类型已知时可以写 `.Warn`、`.Circle(1.0)`。
- 不带数据的枚举满足 `ordered`（按声明顺序），可以转换为整数。

测试：`run/basics`、`run/generics`、`run/errors`、`run/patterns`、`errors/non_exhaustive`。

### 4.4 函数类型与闭包

- `fn(int) int`。闭包字面量 `(x) => x * 2`、`(x, y) => { ... }`，参数和返回类型从期望类型推断；也可以写 `fn(x int) int { ... }`。
- 第 5.6 节规定闭包如何捕获变量。

测试：`run/basics`、`run/generics`、`run/edge`、`run/patterns`。

### 4.5 接口

- 结构化、隐式满足：类型有接口要求的全部方法（名字、参数、`self` 模式都一致）就满足接口。
- 借用接口参数（`r Reader`、`r inout Reader`）是数据指针 + 方法表，不分配内存。
- 接口值（变量、字段、集合元素）是堆上的独占盒子 + 方法表，释放时释放内部的值。接口值永远不为空，可空写 `?Reader`。
- `match r { f File => ..., _ => ... }` 按具体类型分支。
- 接口可以嵌入其他接口。带类型参数的方法和条件方法不计入方法集。
- **类型集**：`interface Num { ~int | ~f64 }`。`~T` 还接受底层类型是 `T` 的新类型。含类型集的接口只能当约束，不能作为值的类型。
- `error` 是内置接口，只有 `fn message(self) String`。

测试：`run/interfaces`、`run/generics`、`run/constraints`、`errors/constraints`、`errors/rules`。

### 4.6 能力

能力是编译器判断的内置约束：

| 能力 | 满足的类型 |
| --- | --- |
| `copy` | 标量、`Span`、copy 结构体和枚举、元素是 copy 的定长数组 |
| `clone` | copy 类型；`String`、`rc`、`arc`、`chan`；元素可 clone 的容器；有 `clone` 方法或字段都可 clone 的类型 |
| `default` | 标量、`String`、`Vec`、`Map`、`?T`；元素有默认值的定长数组；所有字段都有默认值的结构体；底层类型有默认值的新类型 |
| `send` | 不含 `rc`、`weak`、`Cell`、裸指针、函数值的类型（按结构推导）；接口值目前不满足（偏保守） |
| `comparable` | 内置可比较类型；有 `equal` 方法或字段都可比较的类型 |
| `ordered` | 数字、字符串、`bool`、不带数据的枚举、有 `compare` 方法的类型、底层类型 ordered 的新类型 |
| `hash` | 整数、字符串、`bool`；有 `hash` 方法或字段都可哈希的类型 |

测试：`run/generics`、`run/interfaces`、`run/constraints`、`errors/unordered_sort`、`errors/send_rc`、`errors/bad_copy`、`errors/rules`。

## 5. 所有权

### 5.1 move 与 copy

- 每个值只有一个所有者。赋值、传给 `sink` 参数、返回、存进集合、发送到 channel 都转移所有权；之后使用原变量是编译错误。
- copy 类型按位复制，不会被 move。其他类型用 `.clone()` 显式复制。
- 只在部分分支里被 move 的变量，之后的使用是“可能已被 move”的编译错误；编译器为它的释放插入标志位。
- 循环体里不能 move 循环外的变量（下一轮会再 move 一次）。
- 可以 move 结构体的单个字段（部分移动），之后不能再整体使用这个结构体，未被 move 的字段照常释放。
- 不能从借用的位置 move：`return v[0]`（`v` 是借用参数）是编译错误，要写 `.clone()`。

测试：`run/ownership`、`errors/use_after_move`、`errors/maybe_moved`、`errors/move_in_loop`、`errors/partial_move`、`errors/move_out_of_borrow`。

### 5.2 参数模式

| 模式 | 签名 | 调用处 | 规则 |
| --- | --- | --- | --- |
| 只读借用（默认） | `fn show(u User)` | `show(u)` | 函数内不能修改、不能 move 走 |
| 可变借用 | `fn rename(u inout User)` | `rename(&u)` | 实参必须是 `var` 位置，调用处必须写 `&` |
| 转移 | `fn store(u sink User)` | `store(u)` | 调用后实参被 move |

- **独占性**：同一个值在同一时刻要么有任意多个只读借用，要么只有一个可变借用。`both(&a, &a)`、`mixed(a, &a)` 是编译错误。

测试：`run/ownership`、`errors/borrowed_param_mutation`、`errors/inout_needs_amp`、`errors/exclusivity`、`errors/inout_param_invalidation`。

### 5.3 释放

- 所有者离开作用域时释放值。同一作用域的变量按声明的逆序释放，结构体字段按声明顺序释放。
- `fn drop(self inout)` 方法在释放时被调用；有 `drop` 方法的类型不能是 copy。
- `drop(x)` 提前释放，它是一个普通的 `sink` 函数。
- `defer stmt` 在**当前块**结束时执行，多个 defer 逆序执行；`errdefer stmt` 只在函数以错误返回时执行，只能用在返回结果或可选值的函数里。
- `break`、`continue`、`return`、`try` 离开作用域时同样执行释放和 defer。
- 被推迟的代码不能离开它自己：其中不能写 `return`、`try`、`yield`，也不能 `break`/`continue` 外层循环。里面自己的循环、闭包不受影响。

测试：`run/ownership`、`run/errdefer`、`run/labels`、`run/push_flow`、`errors/defer_exit`、`errors/rules`。

### 5.4 视图与投影

- 视图类型 `str`、`[]T`、`MutexRef[T]` 和投影借出的引用**不能存储**：不能做结构体字段、集合元素、会逃逸的闭包的捕获，不能从函数返回（除非来自投影的 `yield`）。
- 投影函数用 `yield` 借出引用：`yield &T`、`yield inout T`、`yield ?&T`。`v[i]`、`m.get(k)`、`m.entry(k)`、`mu.lock()` 都是投影。
- `let x = v[i]`：元素是 copy 时复制出来，否则 `x` 是引用；要拥有值就写 `.clone()`。
- `if let u = m.get(k) { }` 和 `with e = m.entry(k) { }` 把引用绑定到块，引用不能带出块；块内不能修改被借的容器。
- **最后一次使用**：绑定到 `let` 的视图或引用，在块内最后一次使用之后不再占用被借的值，之后可以修改原值。
- 视图不能比它借用的值活得更久；不能从临时值取视图并保存（`let s str = make()`）。
- 存在视图时不能修改被借的 `Vec`/`String`，所以 `push` 不会让视图悬空。

测试：`run/lastuse`、`run/generics`、`run/spans`、`errors/view_field`、`errors/view_return`、`errors/view_temporary`、`errors/view_outlives_owner`、`errors/views_in_containers`、`errors/escaping_view`、`errors/projection_borrow`、`errors/mutable_ref_conflict`、`errors/iterator_invalidation`、`errors/move_while_borrowed`。

### 5.5 共享所有权

- `rc(x)`、`arc(x)` 创建引用计数的值，内部只读；要修改就包 `Cell` 或 `Mutex`。`.clone()` 增加计数。
- `weak[T]` 由 `rc.downgrade()` 得到，`upgrade()` 返回 `?rc[T]`。环由程序员用 `weak` 打破。
- `Arena[T]` 加带代数的 `Id[T]`：用已删除的 Id 访问返回 `none`。

测试：`run/misc`、`run/interfaces`、`run/niche`、`run/arena`。

### 5.6 闭包捕获

- 不逃逸的闭包（作为普通参数传入）借用外层变量，可以修改 `var` 变量。
- 会逃逸的闭包（`sink fn()` 参数、存起来、交给 `go`）拥有捕获的变量：copy 类型被复制，其他类型被 move 进闭包。视图不能被逃逸的闭包捕获。
- 交给 `go` 的闭包不能修改外层变量。

测试：`run/basics`、`run/concurrency`、`errors/escaping_view`、`errors/go_closure_mutation`。

## 6. 表达式

- 运算符优先级从低到高：`??`（右结合）、`||`、`&&`、比较、`|`、`^`、`&`、`<< >>`、`+ - +% -%`、`* / % *%`，然后是一元 `- ! ~ & <- *` 和后缀 `.f`、`?.f`、`(...)`、`[...]`。
- 两边类型必须相同，没有隐式提升。字符串 `+` 拼接。
- `+ - *` 在整数溢出时 panic（debug 和 release 都是）；`+% -% *%` 回绕。`/`、`%` 除零 panic。下标越界 panic。
- `if`、`match`、块都是表达式，值是最后一个表达式。
- `?T` 的操作：`if let v = x { }`、`x ?? d`、`x?.field`（结果仍是可选的，`field` 必须是 copy 类型；其他情况用 `if let`）、`x.unwrap()`（`none` 时 panic）、`match`。
- `[a, b]` 是 `Vec` 字面量，`Map[K, V]{k: v}`、`Vec[T]{}` 是带类型的字面量；全是字符串字面量的列表推断为 `Vec[String]`。
- `a..b` 是半开区间，`a..=b` 是闭区间；`s[a..b]` 对字符串和切片取视图。
- `T ! E` 的结果必须被处理；丢弃写 `_ = f()`。

测试：`run/basics`、`run/edge`、`run/misc`、`run/constraints`、`run/patterns`、`panic/overflow`、`panic/div_zero`、`panic/bounds`、`panic/unwrap_none`、`errors/unhandled_result`、`errors/rules`。

## 7. 语句与控制流

- `if c { } else if d { } else { }`，条件必须是 `bool`。
- `for x in xs`、`for i, x in xs`（下标和元素）、`for i in 0..n`、`for k, v in m`、`for x in ch`、`for cond { }`、`for { }`。
- `for x in v` 借用每个元素；`for x in &v` 可变遍历，`v` 必须是 `var`。遍历期间不能修改被遍历的集合。
- **标签**：`outer: for ...`，`break outer`、`continue outer` 跳到对应的循环。标签不能与外层循环的标签重名；`break` 的标签必须属于外层循环。`select` 分支里的 `break` 作用于外层循环。
- `match` 是穷尽的。模式：字面量（含负数）、区间 `1..=9`、`|` 组合、`_`、绑定、枚举分支（可以嵌套，每个字段都要写）、`ok(x)`/`err(e)`、`some(x)`/`none`、接口值的具体类型 `f File`。分支可以带守卫 `if cond`；带守卫的分支不计入穷尽检查。
- 被匹配的是位置时，绑定默认是引用；分支把绑定 move 走时，这个分支消耗被匹配的值。
- 没有 `switch`。

测试：`run/basics`、`run/labels`、`run/edge`、`run/misc`、`run/patterns`、`errors/labels`、`errors/non_exhaustive`、`errors/iterator_invalidation`、`errors/iter_mutation`、`errors/iter_let`、`errors/rules`。

### 7.1 自定义迭代

`for x in t` 对自定义类型按以下顺序查找协议：

1. **游标（pull）**：`fn cursor(self) C` 和 `fn next(self, c inout C) yield ?&T`（或返回拥有值的 `?T`，用作生成器）。`for x in &t` 用 `nextMut`。`for` 被翻译成“取游标 + 循环调用 `next`”，没有闭包。
2. **推送（push）**：`fn each(self, body fn(&T) Flow)`，`for x in &t` 用 `eachMut(self inout, body fn(inout T) Flow)`。循环体变成不逃逸的闭包；其中的 `break`、`continue`、`return`、`try` 和带标签的跳转被改写成控制信号。`each` 收到 `Flow.Break` 时应该尽快返回；即使它继续调用，循环体也不会再执行，所以写错的 `each` 只会多跑几圈，不会改变程序的结果。循环体会执行多次，所以不能 move 外层变量。

两种协议都提供时用游标。不满足任何协议的类型不能 `for`。遍历期间不能修改被遍历的值。

测试：`run/iterators`、`run/constraints`、`run/push_flow`、`errors/iter_misuse`、`errors/iter_mutation`。

## 8. 错误处理

- `fn f() T ! E`；`return err(e)`、`return x`（自动包成 ok）。
- `try e`：错误时把错误返回给调用方。错误类型必须相同，或者外层的错误类型是 `error`；否则写 `try e else Wrap`，`Wrap` 是把内层错误转换成外层错误的函数或枚举分支。`try` 只能用在返回结果或可选值的函数里；用在 `?T` 上时传播 `none`。
- `e catch err { 块 }`：就地处理，块的值是整个表达式的值。
- `match r { ok(v) => ..., err(.Kind(x)) => ... }`。
- `x.unwrap()` 在出错时 panic。

**panic** 表示程序 bug。panic 打印消息和源码位置（指向用户代码，而不是标准库），以退出码 2 终止整个进程，不展开栈，不执行 defer。没有 `recover`。

测试：`run/errors`、`run/errdefer`、`run/patterns`、`errors/try_error_mismatch`、`errors/try_outside_result`、`errors/unhandled_result`、`panic/unwrap_err`、`panic/unwrap_none`、`panic/no_defer`、`testblocks/blocks`。

## 9. 泛型

- 泛型函数、结构体、枚举：`fn max[T ordered](xs []T) ?T`、`struct Stack[T any]`。约束写在参数旁，或用 `where T: copy + ordered`。
- 方法可以有自己的类型参数；方法上的 `where` 让它成为条件方法，只在条件满足时存在。调用条件不满足的方法是编译错误。
- 类型参数在调用处局部推断（包括闭包的返回类型），无法推断时要写出来：`fresh[int]()`。
- 泛型体里只能使用约束保证的操作：调用接口约束声明的方法、`copy` 才能复制、`ordered` 才能比较、类型集全是数字才能做算术。
- **整数常量泛型**：`struct Mat[R int, C int] { data [R][C]f64 }`。只允许整数参数，类型层面不做运算；维度不匹配是编译错误。
- 实现按类型参数单态化。

测试：`run/generics`、`run/constgen`、`run/constraints`、`errors/constgen`、`errors/constraints`、`errors/unordered_sort`。

## 10. 并发

- `go f(x)`、`go () => { }`：参数和捕获被 move 进新线程，必须满足 `send`。
- `chan[T](n)` 创建带 `n` 个缓冲的 channel，`chan[T]()` 和 `chan[T](0)` 无缓冲。`ch <- v` 发送并 move `v`；`<-ch` 返回 `?T`，channel 关闭后为 `none`；`for x in ch` 一直接收到关闭；`ch.close()`；`.clone()` 得到同一个 channel 的另一个句柄。
- `select` 的分支：`let x = <-ch => ...`、`<-ch => ...`、`ch <- v => ...`、`_ => ...`。没有 `_` 时等待某个分支就绪。分支体里的 `break`/`continue` 作用于外层循环。
- `Mutex(x)` 拥有数据；`arc(Mutex(x))` 用于长期共享。`with v = mu.lock() { }` 加锁，块结束解锁，引用带不出块。
- `Mutex(&x)` 创建借用 `x` 的锁，类型是 `MutexRef[T]`，是视图（不能存储）。存在期间 `x` 被可变借用，不能直接访问；`x` 必须是 `var`。
- `task.group((g) => { g.go(() => ...) })`：块结束时等待所有子任务；子任务可以借用组外的数据；第一个错误作为整个 group 的错误返回。
- 任何线程里的 panic 都终止进程。

测试：`run/concurrency`、`run/mutexref`、`errors/send_rc`、`errors/task_race`、`errors/go_closure_mutation`、`errors/select_arms`、`errors/mutexref`、`errors/mutexref_borrow`、`errors/mutexref_immutable`。

## 11. unsafe 与 FFI

- 解引用 `*T` 和调用 `extern` 函数必须在 `unsafe { }` 里。裸指针可以创建、传递、存进结构体。
- `extern "C" lib "libz" { fn crc32(...) u64 }` 在运行时用 `dlopen` 加载；不写 `lib` 时在 libc 里查找。构建时不需要 C 头文件。`extern "C" static "m" { ... }` 改为静态链接。
- `extern "C" fn name(...) { ... }` 是用 C 符号名导出的 Kite 函数，从 Kite 调用它不需要 `unsafe`。
- `extern "C"` 函数的参数和返回值只能是数字、`bool`、裸指针和只由这些组成的结构体。`String`、`str`、`Vec`、`?T`、函数值等没有 C 表示，声明它们是编译错误。目前还不能把 Kite 函数作为回调传给 C。
- 裸指针让类型不满足 `send`；`unsafe send struct Handle { ... }` 由作者担保它可以跨线程。

测试：`run/ffi`、`run/unsafe_decls`、`errors/unsafe_extern`、`errors/ffi_types`、`errors/send_ptr`。

## 12. 测试块

- `test "name" { ... }` 写在任何文件里，`kite test` 运行它们，`kite build`/`run` 忽略它们。
- 每个测试在单独的进程里运行：一个测试 panic 只让这个测试失败。有失败时退出码是 1。

测试：`testblocks/blocks`。

## 13. 运行模型

- 编译器生成 C，用 `cc` 或 `zig cc` 编译。debug 与 release（`-O2`）的唯一区别是优化级别：溢出检查、下标检查都保留。
- 泛型单态化；IR 节点数不超过 60 的函数带 `inline` 提示。
- `Box`、`rc`/`arc`、接口盒子用按大小分级的线程本地空闲链表分配；`Vec`/`String` 缓冲区用 `malloc`。
- goroutine 目前是操作系统线程。
- `Map` 保持插入顺序。浮点按最短往返表示打印，指数在 [-4, 21) 内时不用科学计数法。
