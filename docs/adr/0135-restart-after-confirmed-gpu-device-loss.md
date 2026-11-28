# ADR 0135: Restart after confirmed GPU device loss

* Status: proposed (2026-11-27)
* Relates to: ADR 0092, Part 1 (mid-session graphics failures), ADR 0105 (window-device submissions),
  ADR 0015 (autosave), ADR 0112 (document tabs), and the recovery extracted from PR #660

## Context

An eframe window can paint once more before it processes a close request. When wgpu has already
reported the window device lost, that paint can panic while creating a staging buffer. Unwinding
through the failed renderer can itself fail, before the existing close-and-reopen path takes over.

## Decision

* A confirmed window-device loss uses a fresh desktop process. Hold the latest tab snapshots
  across the call to the painter, so a panic there after a confirmed loss can use the same handoff.
  Uncaptured errors, a stuck surface, intentional device destruction and unrelated panics keep
  their existing paths.
* Queue each dirty tab's snapshot on its own autosave worker and wait for the result. Report
  failures in the startup log and continue with the copies that succeeded. The replacement can
  reopen clean tabs backed by saved files. The existing Recover window offers unsaved copies.
* Start the same executable with the CPU stage, preferring alternative window APIs. It waits for
  the original process to exit before entering startup. Allow one replacement per process chain.
  The original exits without unwinding the failed graphics stack.
* Use the installed launch profile and its validated root. The replacement goes through normal
  launch validation. Nightly uses the Launcher's selector/root pair, or the existing wrapper form
  for a screenshot run. Place all saved paths after the argument delimiter.
* Finish the original Nightly session before exit, even when spawning the replacement fails.
  Keep any previous interruption that the window has yet to show. The replacement takes the
  profile lease and starts its own session as usual.

## Consequences

If the replacement fails to start, finished recovery copies remain available for a manual
launch. A process that crashes before the handoff, or hangs in the graphics driver, is still
handled by the ordinary startup markers and periodic autosave. In-memory UI state and untitled
tabs without edits are not recreated.
