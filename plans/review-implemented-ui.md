# Integrated UI implementation review

Source-only review of the working-tree diff against F03, F06–F14, F17–F20 and F22–F24 in `plans/qa-final.md`, 11 November 2026. No source edits, builds, tests, native UI execution, subagents or commits. The only written artifact is this report. Reproduction steps below are proposed checks, not observed runtime results. Remote compilation belongs to the validation agent.

Applied Ponytail for narrowly scoped correctness review, Impeccable for UI behavior and accessibility inspection, and Unslop for report wording. No applicable AGENTS.md was found in this checkout or its worktree parent search.

## Findings

### P1 · F17 remains incomplete in the screenshot overlay

Location: `crates/desktop/src/rune_shot.rs:566`, `:581`, `:597`, `:633`.

The new annotation keyboard handler is called only when text editing is already active. With an annotation tool selected, arrows fall through because the region arrow branch requires `session.tool.is_none()`. Enter always saves the screenshot instead of calling the annotation placement handler. The editor canvas has keyboard placement, but the screenshot overlay does not.

Repro: start a region screenshot, use arrows to create a region, press `r` to select Rectangle, then use arrows and Enter to place it. Arrows cannot position the tool and Enter starts screenshot delivery. A keyboard-only user also cannot start the text annotation needed to enter the special text-editing branch.

Fix: route placement/navigation keys to `session.annotation.key` while an annotation tool is active, preserving explicit screenshot output shortcuts. Provide a keyboard route for selecting and nudging existing overlay annotations as well. Verify region movement, rectangle placement and text placement separately.

### P1 · F18/F20 native capture retirement can discard its final event

Location: `crates/desktop/src/rune_capture.rs:315`, `:348`, `:354`. Error producer: `crates/desktop/src/rune_capture.rs:154`.

Native capture drains its event receiver once, then independently checks whether the worker has finished and drops the receiver. A final Error or Finished event sent between those operations is discarded. This is the same retirement race that the integrated `quick_recording::collect_events` helper now handles for screencasts, but the native path still has it. The UI resets capture to Off without reporting the final failure, potentially leaving a stale "Starting capture…" or other prior message.

Repro interleaving: start native recording with an unavailable saved display or a finalization failure; let the first `try_iter` drain finish before the worker sends Error, then let the worker finish before `is_finished`. The event remains queued when `self.events = None` drops it. This interleaving is code-supported and was not induced locally.

Fix: after confirming retirement and joining the native worker, drain and process its remaining events before releasing the receiver. Use a deterministic channel/retirement check like the quick-recorder regression check.

### P2 · F20 capture outcomes still bypass typed notice retention

Location: `crates/desktop/src/rune_capture.rs:337`, `:372`, `:398`; `crates/desktop/src/rune_ui.rs:349`, `:371`, `:529`.

Capture polling still accepts one mutable String and overwrites it for every native event, replay completion and screencast event. The shell can retain only the last value that escapes the call. Its `notice_seen` fallback cannot recover intermediate errors. In addition, ordinary capture success messages bypass `notify`, so `notice_kind()` classifies them as Error and they remain until dismissed.

Repro: run native capture and a screencast concurrently. Have a native Error and quick Saved event ready for the same UI poll. The native error is replaced by the quick save message before the shell sees it, so dismissing notices cannot recover the error. Independently, finish a successful screencast and inspect its notice: the untyped "Saved ..." message is treated as an error and never expires as a success.

Fix: return or emit each capture outcome with an explicit notice kind, then feed each one through the shell queue. Preserve every error before processing later successes. Cover simultaneous completions and a normal capture success.

## Other reviewed changes

No additional concrete defect was established in the reviewed utility mode/default mappings, Split/Index folder pickers, preference rollback and shortcut re-registration, named tray routes, onboarding route, library/disk paging, visible-page thumbnail admission, or disk path/error actions. This is not native acceptance of those features.

The two root fixes named in the review brief are present: reset retains cancelled utility worker handles and Drop joins them; failed result publication retains the generated output for retry. Existing regression checks were read but not run. Utility result/save dialog guards and capture-origin checks were also inspected.

Tests actually run: none. Compilation, Windows capture/device behavior, clipboard/dialog interoperability, keyboard focus dispatch and rendered accessibility remain unverified by this review.
