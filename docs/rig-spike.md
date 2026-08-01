# rig spike — resolves PRD §14.3

**Date:** 2026-08-01 · **Verdict: GO.** Every capability the estimation agent (§5) depends on exists in rig `0.41.0`. The PRD's API names are stale; the design is not.

Verified by reading the vendored crate source, not documentation:
`~/.cargo/registry/src/index.crates.io-*/rig-agent-0.41.0/` and `rig-core-0.41.0/`.

---

## The one structural correction

rig 0.41 shipped a **breaking split** (`rig-core` CHANGELOG, PR #2197):

> `[breaking] split rig-core and rig-agent behind the rig facade`

- `rig-core` — portable contracts only: `Message`, `UserContent`, `CompletionModel`, `PortableTool`, providers. **It has no `Agent` and no run loop.** Grepping `rig-core-0.41.0/src` for `multi_turn|MaxDepth|struct Agent` returns **zero** hits.
- `rig-agent` — the runtime: `Agent`, `AgentBuilder`, `AgentRunner`, hooks, `Extractor`, tool registry.
- `rig` — the facade that re-exports both. **This is what we depend on.**

So the PRD's `rig-core` dependency is wrong. Use:

```toml
rig = { version = "0.41", features = ["agent", "derive"] }
```

`agent` and `derive` are both default-on in the facade; state them anyway so a future default change can't silently remove the runtime.

---

## Capability matrix

| PRD §5 requirement | rig 0.41 reality | Status |
|---|---|---|
| Bounded loop | `AgentRunner::max_turns(n)` — also `AgentBuilder::default_max_turns(n)` | ✅ renamed from `multi_turn` |
| Cap breach → fallback trigger | `PromptError::MaxTurnsError { max_turns }` | ✅ renamed from `MaxDepthError` |
| Typed tools, schema from types | `rig_agent::tool::Tool`: `const NAME`, `type Args: Deserialize`, `type Output: IntoToolOutput`, `type Error`, `fn description()`, `fn parameters() -> serde_json::Value` | ✅ |
| Structured output | `AgentBuilder::output_schema_raw(Schema)` + `output_mode(OutputMode)`; `Extractor` for pure extraction | ✅ |
| **Session persistence** | `Message` derives `Serialize, Deserialize` (`rig-core/src/completion/message.rs:20`) | ✅ **the big one** |
| **Session resume** | `AgentRunner::history<I, T: Into<Message>>(...)` — "Passing explicit history bypasses conversation memory for this run" | ✅ |
| Image input | `UserContent::Image(Image)`; `Image { data: DocumentSourceKind::{Base64,Url}, media_type: Option<ImageMediaType>, detail: Option<ImageDetail> }` | ✅ |
| Per-step audit → `agent_steps` | `AgentHook` (`agent/hook.rs`) with `ToolCall` / `ToolResult` events | ✅ **better than the PRD's plan** |

### `agent_sessions.messages` is now trivial

`Message` is `Serialize + Deserialize`, so persistence is `serde_json` in both directions:

```rust
// finish: persist
let blob = serde_json::to_string(&final_history)?;   // Vec<rig_core::completion::Message>
// reanalyze: resume
let history: Vec<Message> = serde_json::from_str(&blob)?;
let out = agent.prompt(feedback).history(history).max_turns(6).await?;
```

This was the single largest risk in the PRD (§14.3 called it "the one frameworks most often leave underspecified"). It is a non-issue.

### Hooks beat stream-parsing for `agent_steps`

§8 planned to populate `agent_steps` "from rig's multi-turn stream events". `AgentHook` is the better mechanism: register a hook, receive `ToolCall`/`ToolResult` events with typed payloads, write a row per step. No stream parsing, and it works on the blocking path.

---

## Semantic gotcha — `max_turns` counts *model calls*, not tool calls

From `agent/runner.rs:286`:

> Set the total model-call budget, **including the initial call and every retry or continuation**. Zero emits no model calls; one permits only the initial call. Exceeding the budget returns `PromptError::MaxTurnsError`.

The PRD says "max 6 tool calls". That is **not** what `max_turns(6)` gives. A loop that calls two tools and then answers costs 3 model calls. Budget accordingly:

- `max_turns(6)` ≈ up to 5 tool-call rounds plus a final answer — comfortably fits the §5 loop (identify → recall → estimate → critique → emit).
- Do **not** raise it to bound tool calls specifically; if a per-tool-call cap is ever needed, count in an `AgentHook` instead.

Set `default_max_turns(6)` on the builder so every run is bounded even if a call site forgets, and override per-run with `.max_turns()` where a correction turn needs less.

---

## Additional surface worth knowing

- `AgentBuilder`: `preamble`, `append_preamble`, `context`, `temperature`, `max_tokens`, `tool_choice`, `additional_params`, `output_schema_raw`, `output_mode`, `conversation`, `record_content_telemetry`.
- `AgentRunner`: `max_turns`, `history`, `preamble`, `without_preamble`, `document`, `tool_context`, `add_hook`.
- `rig_core::memory` — Rig-managed conversation history keyed by conversation id. **Not used here**: we persist to SQLite ourselves because §5 needs revision-scoped history with a `prompt_version` reseed rule, which Rig's memory does not model.
- `rig_agent::core::*` re-exports `rig_core` items; import portable types through that namespace when depending only on `rig-agent`.
- Feature flags on `rig-core` include `image`, `pdf`, `audio`, `epub` — these gate *loaders and generation*, not vision message content. Vision input needs no feature flag.

## Reproducing this spike

```bash
cargo new /tmp/rigspike && cd /tmp/rigspike
cargo add rig --features derive && cargo fetch
SRC=~/.cargo/registry/src/index.crates.io-*/rig-agent-0.41.0
grep -rn "pub fn max_turns" -A 4 $SRC/src/agent/runner.rs
grep -n "pub trait Tool" -A 30 $SRC/src/tool/mod.rs
grep -n "pub enum Message" -B 3 ~/.cargo/registry/src/index.crates.io-*/rig-core-0.41.0/src/completion/message.rs
```

## Toolchain confirmed on this machine

`rustc 1.96.0` · `node v26.5.0` / `npm 11.17.0` · `openjdk 25` (Zulu) · `docker 29.6.2` · `helm v4.2.3`.

Note: the Dockerfile pins `rust:1.94` per the PRD, but local toolchain is 1.96. Keep the image at or above the local version so a local `cargo build` never produces a lockfile the image can't consume.

Note: JDK 25 locally vs JDK 17 in the PRD's Android build. Gradle toolchain config should pin 17 explicitly so local and CI builds agree.
