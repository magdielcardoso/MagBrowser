# JavaScript

The JavaScript implementation is original: a hand-written lexer, a
recursive-descent parser and a tree-walking interpreter with a mark-and-sweep
garbage collector. No V8, JavaScriptCore, SpiderMonkey or Boa is used.

## Crates

- `browser-js-parser` (`crates/javascript/js-parser`): `tokenize`, `parse`,
  `Program`, `Stmt`, `Expr`, `Function`, positions and `ParseError`.
- `browser-js` (`crates/javascript/js-runtime`): `Runtime`, `Value`, objects,
  functions, scopes, the interpreter, promises and the GC.

## Supported language subset

Statements: `var`/`let`/`const`, blocks, `if`/`else`, `while`, `do…while`,
classic `for`, `for…in`, `for…of`, `switch`, `break`/`continue` (with labels),
`return`, `throw`, `try`/`catch`/`finally`, empty statements, function and arrow
function declarations/expressions.

Expressions: numbers (decimal, hex, binary, octal, exponent), strings with
escapes, template literals with `${}` interpolation, regular expression literals,
booleans, `null`, `undefined`, array and object literals (shorthand properties),
member access, calls, `new`, unary `+ - ! ~ typeof void delete`, binary
arithmetic/comparison, bit shifts (`<<`, `>>`, `>>>`) with 32-bit semantics,
`**`, `&&`, `||`, `??`, ternary, comma, assignment and compound assignment,
`++`/`--`. Destructuring (array/object, nested, defaults, rest, holes) works in
declarations, parameters and assignment expressions; functions support default
and rest parameters. Optional chaining is **not** supported.

Semantics: block-scoped `let`/`const` with temporal dead zone, function-scoped
`var`, hoisting of `var` and function declarations, closures, recursion with a
depth guard, `this` (global in sloppy mode, method receiver in calls),
prototypes (`Object`, `Array`, `Function`, `String`, `Number`, `Boolean`,
`Error`, `Promise`) and `new`, property `get`/`set`/`delete`, numeric and string
indexing, `in`/`instanceof`, exceptions with `try/catch/finally`, and
`for…of`/`for…in`.

Built-ins: `console`, `Math`, `JSON`, `parseInt`, `parseFloat`, `isNaN`,
`Object.keys`, `Array.prototype` (`push/pop/shift/unshift/join/indexOf/slice/
map/forEach/filter/reduce`), `String.prototype` (`charAt/charCodeAt/indexOf/
slice/substring/toUpperCase/toLowerCase/split/trim/replace/includes/startsWith/
endsWith`), `Array.isArray`, `Promise` with `then`/`catch`, `resolve`, `reject`.

Regular expressions are compiled to a small backtracking bytecode VM (literals,
`.`, `\d \w \s`, classes and ranges, `* + ? {n,m}`, lazy quantifiers, anchors,
alternation, capturing and non-capturing groups, `\b`, escapes; flags `g i m s`)
and exposed through `RegExp`, `test`, `exec`, `String.prototype.match/split/
replace` (including `$1`…`$9`, `$&`).

Not supported (documented limitations): classes, generators, `async`/`await`,
getters/setters inside object literals, computed keys, object/array spread,
`for`-head destructuring, regex backreferences and lookaround, `u`/`y` flags,
`eval`, `with`. Promises have no `finally` or thenable assimilation.

## Object model

`Value` is `Undefined | Null | Bool | Number | String | Object(ObjectId)`. Heap
objects live in an arena (`Vec<Option<Object>>` with a free list) and carry a
`kind` (`Ordinary`, `Array`, `Function`, `Promise`), a prototype link, ordered own
properties, and accessor properties (native getter/setter pairs used by the DOM
bindings). Arrays keep a dense element vector; `length` and numeric indices are
synthesised.

Host code registers functions with `Runtime::define_global_function`,
`define_host_method` and `define_accessor`; callbacks receive the runtime, the
`this` value and the arguments, and return `Result<Value, JsError>`.

## Garbage collection

Mark-and-sweep over the object arena:

- Roots: the global object, active scopes/call frames, the microtask queue, and
  values registered with `add_gc_root` (host bindings do this for elements they
  cache).
- Collection is triggered by an allocation threshold and can be forced with
  `collect_garbage`.
- Freed slots are reused; cyclic structures that are unreachable are collected.
- Collection is deferred while microtasks are pending (their captured values are
  not traced yet) — a documented conservative behaviour, not a leak in practice.

## DOM integration

`browser-web-apis` installs `document`, `window`, elements, events, timers,
storage and `location` on a runtime, all backed by the same DOM arena the layout
engine uses. `el.textContent = "x"` mutates the real text node; the caller then
recomputes style/layout/paint, so the change is visible in the next frame.

Execution model today:

1. `browser-core` collects inline `<script>` elements in document order.
2. The runtime is created on first execution and kept on the `Page`, together
   with its binding state, so listeners survive across events.
3. Each script is evaluated; microtasks are drained; due timers run.
4. DOM mutations trigger a full restyle/relayout (correct, not yet incremental).
5. `location.href = …` sets a navigation request that `browser-core` follows.

Testing uses local fixtures only; arbitrary internet scripts are not executed by
the test suite, by design (no process isolation yet).
