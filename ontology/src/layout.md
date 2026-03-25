# Layout

## [Ontology](../../../existence-lang/ontology/src/ontology.md)

A [layout](./layout.md) extends [Pattern](../../../existence-lang/ontology/src/pattern.md). It is a 2D columnar declaration of file paths — the pattern that maps files to intended [pane](./pane.md) positions. A layout is specified as one or more `--col` arguments; each argument is a comma-separated list of file paths forming one column (top-to-bottom). Columns are arranged left-to-right. `Layout::parse` validates that at least one column with at least one file is present.

The layout is pure intent — it describes what the screen should look like without prescribing how to get there. [Reconcile](./reconcile.md) consumes the layout and applies it to the live tmux state.

## [Axiology](../../../existence-lang/ontology/src/axiology.md)

The layout is the user-facing vocabulary of tmux-router. It is how intent is expressed at the CLI. Its columnar structure mirrors the spatial reality of a tiled terminal: files in the same column share vertical space; files in different columns share horizontal space. The pattern is simple enough to type, yet rich enough to describe multi-column, multi-pane workspaces declaratively.

## [Epistemology](../../../existence-lang/ontology/src/epistemology.md)

### [Pattern](../../../existence-lang/ontology/src/pattern.md) Expression

- **CLI form**: `tmux-router --col file1.rs,file2.rs --col file3.rs` — two columns, three panes.
- **Parsed form**: `Layout { columns: vec![vec!["file1.rs", "file2.rs"], vec!["file3.rs"]] }`.
- **File resolution**: each path is resolved via a callback to either `FileResolution::Registered(pane_id)` or `FileResolution::Unmanaged` — the layout drives resolution, not the reverse.
- **Overflow handling**: when panes exceed `MIN_PANE_HEIGHT` (10 rows), `stash_overflow_panes` trims trailing panes column-last, honouring the layout's column order as a priority signal.
- **Size equalisation**: after reconcile, `equalize_sizes` distributes space per column ratios — 50/50 for 2 columns, `even-horizontal` for 3+.
