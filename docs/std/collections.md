# collections

Collections beyond the prelude's `Vec`, `Map` and `Set`: a double-ended
queue, a priority queue, maps and sets kept in key order, and a set of
small integers.

```gojo
var q = Deque[Job].new()            // O(1) at both ends
var pq = Heap[Task].by((a, b) => a.due < b.due)
var m = SortedMap[String, int].new()
for e in m { println(e.key, e.value) }   // in key order
```

**Types:** [`struct Deque`](#struct-deque), [`struct Heap`](#struct-heap), [`struct Entry`](#struct-entry), [`struct SortedMap`](#struct-sortedmap), [`struct SortedSet`](#struct-sortedset), [`struct BitSet`](#struct-bitset)

## Types

### struct Deque

```gojo
pub struct Deque[T] {
    // and private fields
}
```

A double-ended queue: a growable ring buffer, with O(1) pushes and pops at
both ends and O(1) indexing. `for x in d` goes from front to back.

#### Deque.new

```gojo
pub fn new() -> Deque[T]
```

An empty deque.

#### Deque.len

```gojo
pub fn len(self) -> int
```

The number of elements.

#### Deque.isEmpty

```gojo
pub fn isEmpty(self) -> bool
```

Whether there are no elements.

#### Deque.pushBack

```gojo
pub fn pushBack(inout self, x: sink T)
```

Adds `x` at the back.

#### Deque.pushFront

```gojo
pub fn pushFront(inout self, x: sink T)
```

Adds `x` at the front.

#### Deque.popBack

```gojo
pub fn popBack(inout self) -> ?T
```

Removes and returns the back element, or none when empty.

#### Deque.popFront

```gojo
pub fn popFront(inout self) -> ?T
```

Removes and returns the front element, or none when empty.

#### Deque.get

```gojo
pub fn get(self, i: int) yield ?&T
```

Element `i` from the front, or none when out of range.

#### Deque.getMut

```gojo
pub fn getMut(inout self, i: int) yield ?inout T
```

Element `i` from the front, to change in place.

#### Deque.front

```gojo
pub fn front(self) yield ?&T
```

The front element, if any.

#### Deque.back

```gojo
pub fn back(self) yield ?&T
```

The back element, if any.

#### Deque.clear

```gojo
pub fn clear(inout self)
```

Removes every element, keeping the buffer.

#### Deque.toVec

```gojo
pub fn toVec(self) -> Vec[T] where T: clone
```

Copies of the elements, front to back.

#### Deque.cursor

```gojo
pub fn cursor(self) -> int
```

`for x in d`

#### Deque.next

```gojo
pub fn next(self, c: inout int) yield ?&T
```

### struct Heap

```gojo
pub struct Heap[T] {
    // and private fields
}
```

A priority queue (a binary heap): `pop` takes the element that goes first,
in O(log n); `push` is O(log n); `peek` O(1).

```gojo
var h = Heap[int].min()                 // smallest first
var tasks = Heap[Task].by((a, b) => a.priority > b.priority)
```

#### Heap.min

```gojo
pub fn min() -> Heap[T] where T: ordered
```

A heap that pops the smallest element first.

#### Heap.max

```gojo
pub fn max() -> Heap[T] where T: ordered
```

A heap that pops the largest element first.

#### Heap.by

```gojo
pub fn by(first: sink fn(T, T) -> bool) -> Heap[T]
```

A heap that pops first an element `first(a, b)` puts before the others.

#### Heap.len

```gojo
pub fn len(self) -> int
```

The number of elements.

#### Heap.isEmpty

```gojo
pub fn isEmpty(self) -> bool
```

Whether there are no elements.

#### Heap.push

```gojo
pub fn push(inout self, x: sink T)
```

Adds `x`.

#### Heap.peek

```gojo
pub fn peek(self) yield ?&T
```

The element that goes first, without removing it.

#### Heap.pop

```gojo
pub fn pop(inout self) -> ?T
```

Removes and returns the element that goes first, or none when empty.

#### Heap.clear

```gojo
pub fn clear(inout self)
```

Removes every element.

#### Heap.intoSorted

```gojo
pub fn intoSorted(sink self) -> Vec[T]
```

The elements in the order `pop` would give them (the heap is used up).

### struct Entry

```gojo
pub struct Entry[K, V] {
    pub key: K
    pub value: V
}
```

A key and its value, as a sorted map holds them.

### struct SortedMap

```gojo
pub struct SortedMap[K: ordered, V] {
    // and private fields
}
```

A map that keeps its keys in order (a B-tree): lookups, inserts and
removals in O(log n), iteration in key order, and the nearest key above or
below any value. `for e in m` gives each `Entry` (`e.key`, `e.value`).

```gojo
var scores = SortedMap[String, int].new()
_ = scores.insert("bo", 7)
if let e = scores.ceil("b") { ... }      // the first key >= "b"
```

#### SortedMap.new

```gojo
pub fn new() -> SortedMap[K, V]
```

An empty map.

#### SortedMap.len

```gojo
pub fn len(self) -> int
```

The number of entries.

#### SortedMap.isEmpty

```gojo
pub fn isEmpty(self) -> bool
```

Whether there are no entries.

#### SortedMap.get

```gojo
pub fn get(self, k: K) yield ?&V
```

The value for `k`, if any.

#### SortedMap.getMut

```gojo
pub fn getMut(inout self, k: K) yield ?inout V
```

The value for `k`, to change in place.

#### SortedMap.contains

```gojo
pub fn contains(self, k: K) -> bool
```

Whether there is an entry for `k`.

#### SortedMap.insert

```gojo
pub fn insert(inout self, k: sink K, v: sink V) -> ?V
```

Sets the value for `k`, returning the old one.

#### SortedMap.remove

```gojo
pub fn remove(inout self, k: K) -> ?V
```

Removes the entry for `k`, returning its value.

#### SortedMap.popFirst

```gojo
pub fn popFirst(inout self) -> ?Entry[K, V]
```

Removes and returns the entry with the smallest key.

#### SortedMap.popLast

```gojo
pub fn popLast(inout self) -> ?Entry[K, V]
```

Removes and returns the entry with the largest key.

#### SortedMap.first

```gojo
pub fn first(self) yield ?&Entry[K, V]
```

The entry with the smallest key.

#### SortedMap.last

```gojo
pub fn last(self) yield ?&Entry[K, V]
```

The entry with the largest key.

#### SortedMap.ceil

```gojo
pub fn ceil(self, k: K) yield ?&Entry[K, V]
```

The entry with the smallest key >= `k`.

#### SortedMap.floor

```gojo
pub fn floor(self, k: K) yield ?&Entry[K, V]
```

The entry with the largest key <= `k`.

#### SortedMap.clear

```gojo
pub fn clear(inout self)
```

Removes every entry.

#### SortedMap.keys

```gojo
pub fn keys(self) -> Vec[K] where K: clone
```

Copies of the keys, in order.

#### SortedMap.values

```gojo
pub fn values(self) -> Vec[V] where V: clone
```

Copies of the values, in key order.

#### SortedMap.range

```gojo
pub fn range(self, lo: K, hi: K) -> Vec[Entry[K, V]] where K: clone, V: clone
```

Copies of the entries with keys from `lo` up to (not including) `hi`,
in order; O(log n) to find the start.

#### SortedMap.cursor

```gojo
pub fn cursor(self) -> Vec[int]
```

`for e in m`, in key order

#### SortedMap.next

```gojo
pub fn next(self, c: inout Vec[int]) yield ?&Entry[K, V]
```

### struct SortedSet

```gojo
pub struct SortedSet[T: ordered] {
    // and private fields
}
```

A set that keeps its values in order (a B-tree): `for x in s` goes from
smallest to largest.

#### SortedSet.new

```gojo
pub fn new() -> SortedSet[T]
```

An empty set.

#### SortedSet.insert

```gojo
pub fn insert(inout self, x: sink T) -> bool
```

Adds `x`; false if it was there already.

#### SortedSet.remove

```gojo
pub fn remove(inout self, x: T) -> bool
```

Takes `x` out; false if it wasn't there.

#### SortedSet.contains

```gojo
pub fn contains(self, x: T) -> bool
```

Whether `x` is in the set.

#### SortedSet.len

```gojo
pub fn len(self) -> int
```

The number of values.

#### SortedSet.isEmpty

```gojo
pub fn isEmpty(self) -> bool
```

Whether the set is empty.

#### SortedSet.first

```gojo
pub fn first(self) yield ?&T
```

The smallest value.

#### SortedSet.last

```gojo
pub fn last(self) yield ?&T
```

The largest value.

#### SortedSet.ceil

```gojo
pub fn ceil(self, x: T) yield ?&T
```

The smallest value >= `x`.

#### SortedSet.floor

```gojo
pub fn floor(self, x: T) yield ?&T
```

The largest value <= `x`.

#### SortedSet.popFirst

```gojo
pub fn popFirst(inout self) -> ?T
```

Removes and returns the smallest value.

#### SortedSet.popLast

```gojo
pub fn popLast(inout self) -> ?T
```

Removes and returns the largest value.

#### SortedSet.clear

```gojo
pub fn clear(inout self)
```

Removes every value.

#### SortedSet.toVec

```gojo
pub fn toVec(self) -> Vec[T] where T: clone
```

Copies of the values, in order.

#### SortedSet.cursor

```gojo
pub fn cursor(self) -> Vec[int]
```

`for x in s`

#### SortedSet.next

```gojo
pub fn next(self, c: inout Vec[int]) yield ?&T
```

### struct BitSet

```gojo
pub struct BitSet {
    // and private fields
}
```

A set of small non-negative integers, one bit each: membership in O(1),
and set operations a word (64 values) at a time. `for i in s` goes in
increasing order.

#### BitSet.new

```gojo
pub fn new() -> BitSet
```

An empty set.

#### BitSet.insert

```gojo
pub fn insert(inout self, i: int) -> bool
```

Adds `i` (>= 0); false if it was there already.

#### BitSet.remove

```gojo
pub fn remove(inout self, i: int) -> bool
```

Takes `i` out; false if it wasn't there.

#### BitSet.contains

```gojo
pub fn contains(self, i: int) -> bool
```

Whether `i` is in the set.

#### BitSet.len

```gojo
pub fn len(self) -> int
```

The number of values.

#### BitSet.isEmpty

```gojo
pub fn isEmpty(self) -> bool
```

Whether the set is empty.

#### BitSet.clear

```gojo
pub fn clear(inout self)
```

Removes every value.

#### BitSet.unionWith

```gojo
pub fn unionWith(inout self, other: BitSet)
```

Adds every value of `other`.

#### BitSet.intersectWith

```gojo
pub fn intersectWith(inout self, other: BitSet)
```

Keeps only the values also in `other`.

#### BitSet.differenceWith

```gojo
pub fn differenceWith(inout self, other: BitSet)
```

Removes the values in `other`.

#### BitSet.toVec

```gojo
pub fn toVec(self) -> Vec[int]
```

The values, in increasing order.

#### BitSet.cursor

```gojo
pub fn cursor(self) -> int
```

`for i in s`: the cursor is the next value to look at

#### BitSet.next

```gojo
pub fn next(self, c: inout int) -> ?int
```

