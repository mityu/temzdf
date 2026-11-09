# temzdf sample Markdown

A paragraph with *emphasis*, **strong text**, `inline code`, ~~strikethrough~~
and a [link to the PDF section](#pdf-viewing). A bare URL: https://example.com/docs.
日本語の文も入れます。全角の文字は二桁の幅です。

## Lists

- First item
- Second item with a longer text that should wrap when the pane is narrow enough to need it
  - Nested item
  - Another nested item
- [x] A done task
- [ ] An open task

1. One
2. Two
3. Three

## Tables

| Feature | Text mode | Graphics mode |
|:--------|:---------:|--------------:|
| Tables  | yes       | yes           |
| Math    | utftex    | RaTeX         |
| Images  | if kitty  | yes           |

## Code

```rust
fn main() {
    let greeting = "hello";
    println!("{greeting}, world");
}
```

## Math

Inline math $e^{i\pi} + 1 = 0$ sits in the text, and a display formula:

$$
\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}
$$

## Quotes and alerts

> A quoted paragraph.

> [!NOTE]
> An alert, as GitHub writes it.

## PDF viewing

An image:

![A figure](sample-figure.svg)

A footnote reference[^1].

---

[^1]: The footnote text.
