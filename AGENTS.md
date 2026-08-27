<!-- tsift:code-navigation v=0.1.93 -->
## Code Navigation

Run `tsift status` at session start from the owning repo root. If the task or file lives under a git submodule (for example `src/tsift/...`), switch to that submodule root first so the harness loads the narrower local instructions and repo state instead of the superproject root. `tsift status` repairs the `.tsift/` index state it owns and never rewrites tracked files (`--no-fix` skips even that). If status reports stale or missing instructions, run `tsift init` to refresh the tracked Code Navigation block and runbook; it names every tracked file it rewrites or moves. When the harness cannot perform write commands, ask the user to run the printed `run:` command instead.

Prefer tsift envelopes over raw reads:
- `tsift --envelope search <query>` instead of `grep`/`rg`
- `tsift --envelope source-read <file>` / `tsift --envelope symbol-read <symbol>` instead of raw `cat`/`head`/`tail`/`sed`/`less` source reads
- `tsift --envelope explain <symbol>` and `tsift graph <symbol> --callers` / `--callees` for call graphs
- `tsift diff-digest [path]` (`--pathspec <pathspec>` to preserve scoped reviews) instead of `git diff`, commit-form `git show`, or patch-style `git log`; blob-form `git show <rev>:<path>` stays a raw object read
- `tsift --envelope session-review <path>` / `tsift --envelope context-pack <path>` instead of replaying long session docs or transcripts
- raw-read rewrites route recognized session docs/transcripts to `tsift session-digest --input <path>` and captured logs to `tsift log-digest --input <path>`
- `tsift --envelope digest-runner --kind test|log --path . --shell-command '<command>'` instead of raw test/build output

Command detail lives in [`.agent/runbooks/code-navigation.md`](.agent/runbooks/code-navigation.md) — budgets, `tsift workflow search`, `report.scale_guard` handling, the harness rewrite path for `PreToolUse`-less harnesses, and Codex/OpenCode integration. `tsift init` writes and versions that runbook alongside this block, so it is present in every initialized checkout; read it before broad exploration instead of expanding this block. A repository that also ships a current `.claude/skills/tsift/SKILL.md` should use that skill as the deeper source.

After local changes, check the latest GitHub Actions CI run with `gh run list --limit 1` and fix any failing tests before calling the work complete.

Only read full source files when tsift results are insufficient.
<!-- /tsift:code-navigation -->
