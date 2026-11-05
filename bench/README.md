# 性能对比：Kite vs Go vs Zig

8 个常见基准（大部分来自 Benchmarks Game），每个都用三种语言写了算法和输入完全相同的实现。`run.sh` 先检查所有版本的输出逐字节一致，再用 hyperfine 测量。

```sh
./run.sh            # RUNS=10，结果写入 results.md
```

## 环境

Apple M5（10 核）、32 GB 内存、macOS。Kite 原型（C 后端，Apple clang 21，`--release` = `-O2`）、Go 1.27.3、Zig 0.17.0。程序本身都是单线程；Go 的 GC 会在其他核上并行运行。Zig 使用系统 `malloc`（`std.heap.c_allocator`）。

- **Kite `--release`**：整数溢出检查、下标越界检查都开着（Kite 的默认行为）。
- **Go**：有下标越界检查，整数静默回绕。
- **Zig ReleaseFast**：不做任何检查；**Zig ReleaseSafe**：溢出和越界检查都开着，安全级别和 Kite 最接近。

## 运行时间

hyperfine 10 次运行的平均值，越小越好。

| 基准 | 考察点 | Kite | Go | Zig ReleaseFast | Zig ReleaseSafe |
| --- | --- | --- | --- | --- | --- |
| fib(40) | 函数调用 | 247 ms | 237 ms | **152 ms** | 248 ms |
| binary-trees(18) | 大量小对象分配 | **245 ms** | 669 ms | 957 ms | 963 ms |
| nbody(5M 步) | 浮点、结构体 | 150 ms | 155 ms | **98 ms** | **98 ms** |
| spectral-norm(2000) | 浮点、嵌套循环 | 106 ms | 134 ms | **101 ms** | 102 ms |
| fannkuch-redux(10) | 整数数组 | 129 ms | 123 ms | 116 ms | **110 ms** |
| mandelbrot(2000) | 纯浮点循环 | **102 ms** | 161 ms | 141 ms | 138 ms |
| hashmap | 整数键 / 字符串键 | **80 ms** | 92 ms | 93 ms | 102 ms |
| sort(5M 整数) | 排序 | **260 ms** | 309 ms | 380 ms | 429 ms |

几何平均：Kite 的耗时是 Go 的 **0.78 倍**、Zig ReleaseSafe 的 **0.80 倍**、Zig ReleaseFast 的 **0.87 倍**。binary-trees 的差距主要来自分配器（见下文），不计它时分别是 0.87、0.94、1.03 倍。

这些程序都是单线程的，但 Kite 程序已经运行在 M:N 调度器上（`main` 是第一个 goroutine），所以这张表也说明调度器不给不用并发的代码增加开销。

## 做过的优化

第一轮测量时 Kite 是 Go 的 1.10 倍、Zig ReleaseSafe 的 1.09 倍。对照实验（在生成的 C 里去掉检查、换写法再测）定位出下面这些原因，都已修复：

| 优化 | 影响的基准 | 之前 | 之后 |
| --- | --- | --- | --- |
| 小函数加 `inline` 提示 | spectral-norm | 152 ms | 100 ms |
| 按元素类型生成排序 | sort | 415 ms | 259 ms |
| 按键类型生成 Map 查找；更快的字符串哈希和整数格式化 | hashmap | 107 ms | 79 ms |
| `?Box[T]` 等用空指针表示 none | binary-trees | 938 ms | 887 ms |
| `Box`/`rc`/`arc`/接口盒子用分级空闲链表分配 | binary-trees | 887 ms | 268 ms |
| 空闲链表直接读线程寄存器定位（不再经过 macOS 的线程局部变量函数） | binary-trees | 268 ms | 245 ms |

- **溢出检查不贵，没被内联才贵。** spectral-norm 的热点 `a(i, j)` 只有一行，但带着 5 个溢出检查的 panic 分支，clang 判断它太大，没有内联进内层循环。加上 `inline` 提示后，带全部检查的 Kite 和 Zig ReleaseSafe 一样快（100 vs 99 ms）。去掉检查只会再快 4 ms。现在编译器对类型化 IR 节点数不超过 60 的函数都加这个提示。
- **fib 的差距是溢出检查本身。** 递归的 `fib(n-1) + fib(n-2)` 没法证明不溢出：Kite 247 ms 和 Zig ReleaseSafe 246 ms 一样，去掉检查后是 159 ms，接近 Zig ReleaseFast。这是"release 也默认检查溢出"的代价，可以考虑按包关闭检查，或者在后端消掉能证明不会溢出的检查。
- **排序**：原来是通过函数指针比较、`memcpy` 搬元素的通用归并排序。现在按元素类型生成代码：整数、浮点、字符串用 introsort（比较直接内联；相等元素的先后顺序对这些类型不可观察）；其他类型用同样按类型生成的稳定归并排序，比较是直接调用。
- **Map**：查找、插入、删除按键类型生成，哈希和相等比较内联，不再走函数指针；扩容和重建索引只依赖已存的哈希值，仍是运行时里的通用代码。整数键部分已经比 Go 快（53 vs 61 ms）；字符串哈希改成每次处理 8 字节，`fmt.sprint(int)` 去掉了 `snprintf`（单次 45 ns → 20 ns）。
- **`?Box[T]`、`?rc[T]`、`?arc[T]`、`?weak[T]` 只占一个指针**，none 就是 NULL（设计文档的承诺）。二叉树节点从 32 字节降到 16 字节。
- **小对象分配器**：Kite 的所有权规则让编译器在每个释放点都知道对象的确切大小，所以 `Box`、`rc`/`arc` 和接口值的堆盒子（≤ 256 字节）改用按 16 字节分级的线程本地空闲链表，从 64 KB 的块里切分，不再每次走 `malloc`/`free`。`Vec`/`String` 的缓冲区仍用 `malloc`/`realloc`。用 AddressSanitizer 构建时自动退回 `malloc`/`free`，内存安全测试仍然有效。
- **线程局部变量与 goroutine 迁移**：goroutine 可能在另一个线程上恢复，而 C 编译器会把线程局部变量的地址在一个函数里缓存下来，跨过一次可能切换线程的调用继续用。所以生成的代码每次都用 volatile 内联汇编重新读线程寄存器（Linux 上加固定偏移，macOS 上读 pthread 键槽），和 Go 运行时的做法相同。这比原来 macOS 上经过 `tlv_get_addr` 的访问还快。

## 还没解释的差距

- **nbody**：Kite 和 Go 持平（151 vs 150 ms），Zig 是 98 ms。部分原因是 Zig 版本用了编译期定长数组，循环可以完全展开；把 Zig 改成运行时长度的切片后是 122 ms。剩下约 20% 试过以下改动都无效：去掉溢出 / 下标检查、换用 `zig cc` 编译同一份 C、`-O3`、去掉 `-fwrapv`、把 Vec 头读到局部变量、关闭 FMA 融合。
- **binary-trees 的对比要看分配器**：Zig 换成内存池或 arena 分配器也能大幅提速。这一项说明的是 Kite 不用改程序就能拿到这种分配器，而不是语言本身更快。
- **mandelbrot** 上 Kite 最快，只是 clang 对这段循环生成的代码恰好更好，不代表语言本身有优势。

## goroutine

并发程序单独测量（Zig 没有对应物）：

```sh
kite build --release kite/goroutines.kite -o /tmp/k && /usr/bin/time -l /tmp/k   # Linux 上用 time -v
(cd go && go build -o /tmp/g ./goroutines) && /usr/bin/time -l /tmp/g
```

| 测试 | 平台 | Kite | Go |
| --- | --- | --- | --- |
| 10 万个同时挂起的 goroutine 接力传值（`goroutines`） | Linux arm64（容器） | 0.38 s，441 MB | 0.11 s，284 MB |
| 同上 | macOS（Apple M5） | 0.68 s，1.67 GB | 0.39 s，293 MB |
| 100 万个短命 goroutine，每批 1000 个（`spawn`） | macOS | 382 ms，74 MB | 278 ms |

- **内存**：Kite 的 goroutine 栈不能移动（任务可以借用别的 goroutine 栈上的数据），所以不能像 Go 那样从 2 KB 起步再整体复制扩容。每个栈是一段按需占页的虚拟地址，一个挂起的小 goroutine 占一页：Linux 4 KB，Apple Silicon 16 KB。这就是 macOS 上 1.67 GB 的来源。
- **映射数**：Linux 6.13 起用 `MADV_GUARD_INSTALL` 设保护页，相邻的栈合并成一个映射；更早的内核每个 goroutine 占两个映射，受 `vm.max_map_count`（默认 262144）限制，约 13 万个 goroutine 时报错并提示调大这个值。
- **栈的复用**：结束的 goroutine 把栈留在工作线程的空闲列表里，多了就分给其他线程；共享池超过 4096 个时才把内存还给系统。最初每次都 `madvise`，`spawn` 要 1.16 s。

## 编译时间

以 nbody 为例，每次测量前改动源文件，让编译缓存失效（Kite 清掉二进制缓存，保留已编译的运行时库）：

| 命令 | 时间 |
| --- | --- |
| `kite check`（解析、类型检查、借用检查） | 3 ms |
| `kite build`（debug，生成 C + `cc -O0`） | 51 ms |
| `kite build --release`（`cc -O2`） | 65 ms |
| `go build` | 97 ms |
| `zig build-exe`（Debug） | 924 ms |
| `zig build-exe -O ReleaseFast` | 3.8 s |

Kite 前端只占约 3 ms，其余是 C 编译器的时间。源码没变时再次构建会直接用缓存的二进制。
