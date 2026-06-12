# Opt-out markers

Opt-out markers are small comments you place directly in your source to control how
SAI treats a piece of code. They give you per-function (or per-window) control over two
independent things: whether a chunk is indexed at all, and whether it participates in
near-duplicate clustering.

There are exactly two markers:

| Marker | Effect |
| --- | --- |
| `sai-noindexing` | The chunk is **skipped entirely** — never embedded, never stored. It will not appear in search (`sai_search_code`), nor in `sai_find_similar` or `sai_find_duplicates`. |
| `sai-noduplicate` | The chunk is **still indexed and searchable**, but is **excluded from near-duplicate clustering** (the `duplicates` command / `sai_find_duplicates` MCP tool). |

Use `sai-noindexing` to keep code out of the index completely (vendored snippets,
intentionally noisy boilerplate). Use `sai-noduplicate` when code *should* stay findable
by search but you do not want it flagged as a duplicate — for example, deliberately
parallel test fixtures or per-language mirrors of the same routine.

## How detection works

Detection is a **case-insensitive substring match** on the raw source — the original
file text, *before* comment stripping. Because it is a plain substring scan, it is
language- and comment-syntax agnostic. All of these are detected:

```typescript
// sai-noindexing
```

```python
# sai-noduplicate
```

```sql
-- sai-noindexing
```

```html
<!-- sai-noduplicate -->
```

The marker only has to appear *somewhere* inside the chunk's line span; it does not need
to be on its own line, and casing is ignored (`sai-NoIndexing`, `SAI-NODUPLICATE`, and
`sai-noindexing` all match).

## Granularity

What a single marker affects depends on the chunker (see
[`chunking.md`](../reference/chunking.md)):

- **AST chunker** (`chunker: ast`, covering TypeScript/TSX, Rust, and Go): each function
  is one chunk, so a marker applies to **the function whose body contains it**. Place the
  marker on the line above the function or anywhere inside it.
- **Lines chunker** (`chunker: lines`, the default and the fallback for every other
  language): chunks are line windows (up to ~60 lines, with 8-line overlap), so a marker
  applies at **window granularity**. A marker near a window boundary may only drop the
  window(s) whose line range contains it — the overlapping neighbour window can survive.

For predictable results, prefer the AST chunker when your code is TS/TSX/Rust/Go, and
put the marker right next to the function it should govern.

## Examples

`sai-noindexing` — the function is never embedded or stored:

```typescript
// sai-noindexing
function internalHelper() {
  // This function is never indexed, so it can't be searched or matched.
}
```

`sai-noduplicate` — the function stays indexed and searchable, but is excluded from
duplicate clustering:

```typescript
// sai-noduplicate
function intentionallySimilar() {
  // Findable by search; never grouped into a duplicate cluster.
}
```

## Disabling marker handling

Both markers are honored by default. Two config toggles let you turn them off
independently (both default to `true`):

```yaml
honor_noindex_marker: true        # respect sai-noindexing comments
honor_noduplicate_marker: true    # respect sai-noduplicate comments
```

Set `honor_noindex_marker: false` to index even chunks containing `sai-noindexing`, and
`honor_noduplicate_marker: false` to include `sai-noduplicate` chunks in duplicate
clustering. When a toggle is off, that marker is ignored — the other toggle is
unaffected. See [`configuration.md`](../reference/configuration.md) for where these keys
live in `sai-cfg.yml`.

## Caveat: string literals also trigger the markers

Because detection is a raw substring match (it runs *before* comments are stripped, and
it does not parse the code), the literal marker text inside a **string literal** triggers
the opt-out just like a comment would. For example:

```typescript
function logUsage() {
  // Oops: this string literal contains the marker text verbatim.
  console.log("the sai-noindexing flag was set");
}
```

The function above would be skipped from indexing even though no comment marks it. Avoid
embedding the literal strings `sai-noindexing` or `sai-noduplicate` in code unless you
actually intend the opt-out.

## Related pages

- [Chunking reference](../reference/chunking.md) — how functions and line windows become chunks.
- [Configuration reference](../reference/configuration.md) — the `honor_noindex_marker` / `honor_noduplicate_marker` keys.
- [Search and duplicates](./search-and-duplicates.md) — what duplicate clustering does and which tools it powers.
