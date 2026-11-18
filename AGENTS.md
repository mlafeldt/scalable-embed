# scalable-embed

Fork of [Scalable CLI](https://github.com/ScalableCapital/scalable-cli). `main`
is upstream's releases plus the fork's commits, and every upstream release tag
gets merged in. Each upstream line the fork modifies can conflict on that merge,
so the fork keeps its footprint in upstream files minimal.

## Where code goes

- Fork code: `src/embed.rs`, `src/embed/`, `embed/` and `tests/embed.rs`.
- Code that needs none of upstream's internals: the `embed/` crate. It has a
  `Cargo.lock` of its own and can take any dependency.
- Fork docs: `.github/README.md`. The root `README.md` is upstream's.

## Hooks

Every other file is upstream's and changes only through a **hook**: the fewest
lines that let `src/embed` reach in, such as a call into `crate::embed` or a
widened visibility.

- Put the logic in `src/embed`; the hook only calls it.
- Prefer adding lines to modifying upstream's. Modified lines are the ones that
  conflict.
- Leave every other upstream line byte-identical, even where it could be
  formatted, renamed or fixed.
- The root package takes no new dependencies. Upstream changes the root
  `Cargo.lock` with every release, so an added one conflicts on each merge.
- The hooks table in `.github/README.md` (section "Layout") lists every hook.
  Update it with each hook you add, change or remove.
