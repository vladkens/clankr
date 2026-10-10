# Markdown: wrapping, code, formulas, and images

Local paths in this example assume clankr is running from the project root.

## Long paragraph and formatting boundaries

This paragraph deliberately spans several screen rows: it starts with plain text, followed by **a bold fragment with *italics inside***, then more plain text, and near the end comes [**a long link label pointing to the Markdown renderer's source code, which may also cross the terminal's right edge**](src/markdown/mod.rs). Underlining should stop immediately after the link. There is also ~~an outdated version~~, `inline_code`, Cyrillic, 界, a combining character é, and an emoji 👩‍💻. When narrowing and widening the window, words should keep their order, and copying the paragraph should not introduce extra newlines from screen wrapping.

Plain text. **Bold.** Plain. *Italic.* Plain. ~~Strikethrough.~~ Plain. [Link](src/main.rs). Plain. [`Code inside a link`](Cargo.toml). Plain.

\*These are literal asterisks\*, \_these are literal underscores\_, and `**this is not bold text**`. Backticks inside code: ``the value `name` is literal here``.

## Languages and character widths

Этот абзац проверяет кириллицу: съешь ещё этих мягких французских булок, да выпей чаю. При изменении ширины окна слова должны сохранять порядок, а выделение мышью — исходный текст.

这句话用于检查中文显示、自动换行和鼠标选择，调整终端宽度后文字顺序应保持不变。

この文章は日本語の表示、折り返し、マウスでの選択を確認し、端末の幅を変えても文字の順序が変わらないことを確かめるためのものです。

## Nested blocks

1. First item with several paragraphs.

   The second paragraph still belongs to the first item.

   - Nested item.
     - Another level with **emphasis**.
     - Adjacent item at the same level.

   > A block quote inside the first item.
   >
   > The quote's second paragraph with a [link](src/main.rs).

   ```rust
   let robot = "clankr";
   println!("roger roger, {robot}");
   ```

2. The second item is back at the top level.
3. The third item follows the nested blocks.

> Outer block quote.
>
> > Nested block quote with *italics*.
>
> Back to the outer block quote.

- [x] Check a completed item.
- [ ] Check an incomplete item.
  - [x] Check a nested item with `code`.

## Table with different character widths

| Left | Center | Right |
| :--- | :---: | ---: |
| Rust | **ready** | 42 |
| Cyrillic | 👩‍💻 | 1 024 |
| 界 | é | 7 |
| [main.rs](src/main.rs) | `a \| b` | 123 456 |
| | empty on the left | |
| This cell is deliberately long to test the table in a narrow window | *test* | -0.25 |

The table is followed by an ordinary paragraph without indentation or cell formatting.

## Code in different languages

### Rust

```rust
#[derive(Debug)]
struct Robot<'a> {
  name: &'a str,
  charge: u8,
}

fn main() {
  // A comment, strings, numbers, types, and a macro.
  let robot = Robot { name: "clankr", charge: 42 };
  println!("{:?}", robot);
}
```

### Python

```python
from dataclasses import dataclass

@dataclass
class Robot:
    name: str
    charge: int = 42

def greet(robot: Robot) -> str:
    """Return a greeting without waking the other robots."""
    return f"roger roger, {robot.name}: {robot.charge}%"

print(greet(Robot("clankr")))
```

### TypeScript

```typescript
type Robot = { name: string; charge?: number };

async function greet(robot: Robot): Promise<string> {
  // Template string, optional field and nullish coalescing.
  const charge = robot.charge ?? 100;
  return `roger roger, ${robot.name}: ${charge}%`;
}

console.log(await greet({ name: "clankr", charge: 42 }));
```

### Bash

```bash
#!/usr/bin/env bash
set -euo pipefail

robot="clankr"
for charge in 100 42 0; do
  printf '%s: %s%%\n' "$robot" "$charge"
done

# Markdown markers inside code must stay literal.
printf '%s\n' '**not bold**' '[not a link](src/main.rs)' '$x^2$'
```

### JSON

```json
{
  "name": "clankr",
  "charge": 42,
  "awake": true,
  "tasks": ["lint", "test", "nap"],
  "supervisor": null,
  "greeting": "roger\nroger"
}
```

### SQL

```sql
-- Keywords, a string, an aggregate and a number.
SELECT language, SUM(lines) AS total
FROM source_files
WHERE repository = 'clankr' AND lines > 0
GROUP BY language
ORDER BY total DESC
LIMIT 10;
```

### Diff

```diff
-let charge = 0;
+let charge = 42;
```

### Block without a language

```
    Four-space indentation.

    **Do not highlight**, _do not italicize_, [do not turn into a link](src/main.rs).
    👩‍💻 界 é
```

### Unknown language

```robotlang
ROGER ROGER
IF battery < 42 THEN request_coffee(human)
```

### Markdown inside Markdown

````markdown
# This is code block content

```rust
println!("Triple backticks do not close the outer block");
```

**These asterisks should remain visible.**
````

Plain text resumes after all the blocks: code formatting must not leak into this paragraph.

## Formulas

Inline formulas: $E = mc^2$, $a_i^2 + b_i^2 = c_i^2$, and $\frac{1}{n}\sum_{i=1}^{n} x_i$. Plain text resumes after each formula.

The quadratic formula in a separate block:

$$
x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}
$$

A matrix and multiple lines:

$$
\begin{aligned}
A &= \begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix} \\
\det(A) &= 1 \cdot 4 - 2 \cdot 3 = -2
\end{aligned}
$$

Alternative LaTeX delimiters: \(\alpha + \beta = \gamma\).

\[
\int_0^1 x^2\,dx = \frac{1}{3}
\]

A formula in a block with the `math` language:

```math
f(x) = \begin{cases} x^2, & x \ge 0 \\ -x, & x < 0 \end{cases}
```

These are ordinary prices and names, not formulas: $5 for coffee, $10 for oil, `price_usd`, `cargo_home`, and `a_b_c`.

## Images

A local image with a description:

![The clankr robot with an antenna and two eyes](mocks/robot.svg)

The same image, linking to a source file:

[![Open main.rs through the robot image](mocks/robot.svg)](src/main.rs)

An image with empty alt text:

![](mocks/robot.svg)

A deliberately missing image with a meaningful description:

![A spare battery was supposed to be here](mocks/missing-battery.png)

Plain text resumes after the image. The input field and next message must remain available whether or not the terminal supports images.

---

**End of example.** Type a new message, check Ctrl+C, and copy several paragraphs with the mouse.
