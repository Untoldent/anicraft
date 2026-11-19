# Array panel — interaction design brief

**Status:** working design brief for the experimental Array branch; not a product-wide policy.
**Scope:** the Array editing experience, its session semantics, and stage controls. Follow
[AGENTS.md](../AGENTS.md), [product principles](product-principles.md), and
[UI guidelines](ui-guidelines.md) for implementation conventions.

## The idea

An Array operation makes a spatial arrangement of ordinary, independent AniCraft
instances or artwork. It interpolates properties *across generated instances* (not
across timeline frames) using draggable property stops. The final result is baked,
not a persistent procedural object. In this first iteration, only a selected
symbol instance is supported.

Preserve the existing `array.create` engine command and its inclusive count
semantics: **10 means 10 total instances, including the original.** Instance 1
repositions the original and retains its identity. The preview is reversible;
Commit records one undo step and selects the result.

## A deliberate mode, not an always-active panel

An Array panel appears in AniCraft's normal dock system. The topmost setting is:

**Array type:** `None` (default), `Linear`, `Grid`, `Radial`, `Along Path`.

Only Linear and Radial are implemented initially. Present Grid and Along Path as
unavailable, rather than pretending they generate anything.

- **None** means no active Array editing transaction. The panel may remain open.
  No stage preview or Array manipulation handles are active.
- Selecting a supported type with an eligible source selection **enters Array mode**.
  Preview generation starts immediately. Show **Commit** and **Cancel**.
- Changing between supported types does **not** exit Array mode, change the
  source selection, discard property stops, or erase geometry settings held for
  the other type. Regenerate from the original snapshot, not from preview copies.
- **Commit** keeps the baked artwork, ends the gesture as one undo step, returns
  the dropdown to None, and retains the parameter configuration for later use.
- **Escape**, **Cancel**, or changing the dropdown to **None** abandons the
  preview and restores the pre-entry document and selection without an undo step.
  These exits also return the dropdown to None, **but keep the parameter
  configuration** so the artist can re-enter Array mode without reconstructing it.
- A hidden, closed, or backgrounded Array panel must **not silently discard** an
  active session. Make the active operation discoverable; design safe navigation
  and explicit exit. Avoid hijacking Escape from an open child popup or text field:
  normal UI dismissal should take precedence where necessary.
- If other editing actions conflict with the active preview, do not permit them
  to destroy the transaction. Treat array preview as an explicit editing session.
- The mode's geometry and property configuration is **view state** and is not
  itself written into the document or undo stack.

### Remembering versus resetting

Keep last-used parameter values (including property stops) after Commit or Cancel.
Remember independent geometry for each array type. **Do not** remember a document
snapshot, live preview instances, or the previously selected object as a future
source. The next session must bind to the current eligible selection.

Offer a distinct **Reset Settings to Defaults** action. This resets remembered
stops and geometry and is separate from Cancel. Because it can erase elaborate
stop configurations, ask for confirmation before overwriting customized values;
do not reset the document or break the preview transaction. The exact reset
button position can be refined during UI review. Remembered settings across
an app restart are a future enhancement; retention within the running session
is required now.

## Geometry: interactive stage handles

**Linear:** the controlling metaphor is a **barbell**:

- A draggable **Start** circle is the registration point of instance 1.
- A draggable **End** circle is the registration point of the last instance.
- A visible dynamic line connects them and shows the direction/extent.
- Drag either circle to update Start/End X/Y, total distance, direction, and
  derived per-copy spacing immediately in the panel.
- Drag the line itself to translate both endpoints together.
- The last instance occupies the end point when count is greater than one.
  For count = 1, only the Start influences the result; handle the End visibly
  but without dividing by zero.
- Endpoint coordinates are the *authoritative* linear geometry representation.
  Per-copy spacing is derived by dividing each axis' extent by (count - 1).
  If editable Spacing X/Y are exposed, editing them moves the End accordingly.
  Changing count keeps the two endpoints fixed and recomputes spacing.
- Intermediate-copy positions may appear as small ticks/dots on the barbell line;
  keep artwork legible.

**Radial:** extend the same direct manipulation metaphor:

- Draggable **Centre/Fulcrum** moves the orbit of all copies together.
- Draggable **Start** on the circumference controls radius and starting angle.
- Draggable **Arc End** controls sweep angle (and must distinguish arc direction
  where appropriate). Show the circular arc and tick marks for instances.
- Numeric Centre X/Y, Radius, Start Angle, and Sweep Angle remain synchronized.
- A closed 360° array uses distinct positions, not an overlapping duplicate at
  the start/end seam.
- Keep each instance's *orbit* separate from its own *rotation*.

**Implementation order:** the editing-mode lifecycle and neutral stop track
come first; implement the stage handles in the following reviewed increment,
rather than treating numerical fields as the final design.

## Dim Stage

**Dim Stage** is a checkbox, **checked by default** during Array mode.
Unrelated artwork is visually subdued; the source and generated preview remain
fully saturated. This is an editor-only presentation, never a change to colour
effects, document contents, or exports.

The artist can turn it off to inspect true colours, alpha, blend, filters, and
overlap against the unmodified surrounding artwork.

## Property stops — reuse the existing gradient interaction, not its colour meaning

Use the existing gradient stop bar's interaction conventions and, ideally, its
shared implementation: add/select/drag/reorder/remove stop handles, keyboard
nudge, editable stop position, and the selected-stop model.

However, the **Array ramp is not a colour gradient**. A stop may eventually
capture transform, opacity, tint/brightness/advanced colour effects, filter
parameters, playback offset for Graphic symbols, stacking depth, and easing.

- Show a **neutral horizontal track**, with thin vertical lines at the stop
  positions and a distinct selected marker. Do not encode Alpha as a blue
  colour-gradient preview or imply other properties are summarized as a colour.
- Beneath the track are **context-sensitive collapsible sections** for editing
  properties in the selected stop (first iteration: Colour Effect > Alpha).
- Preserve independently draggable stops at positions 0..100%. Example:
  positions 0%, 40%, 100% can have Alpha 75%, 100%, 3%; dragging the 40% stop
  changes how quickly that middle value is approached.
- The original source's filter list provides the filter *types and order*.
  Future stops may alter those existing filter values but do not add/delete
  filter types.
- Future multiple-property stops share one set of stop positions; optional
  easing applies between stops.
- Future z-order interpolation can sample numeric depths and then sort copies
  stably, since visual stacking is discrete.
- A radial alpha progression of 100% -> 0% -> 100% can be achieved with three
  stops. Do not impose aesthetic loop-correction rules.

## Future distribution modes (not in this milestone)

- **Grid:** count X and Y, spacing, optional staggering; property progression
  direction can be Horizontal (left->right), Vertical (top->bottom), or Diagonal
  (top-left->bottom-right), with reversal later.
- **Along Path:** distribute evenly by arc length, by equal path parameter
  intervals, or at Bézier anchor **nodes**. Closed paths avoid duplicated
  initial nodes. Path selection and tangent alignment need later UI work.
- **Chaos:** a global intensity slider (0..100%) with property categories
  such as position, rotation, scale and Alpha; deterministic random seed so
  editing stops doesn't reshuffle unrelated positions.
- Source can eventually be multiple selected instances, a group, a drawing
  object, or mergeable raw artwork. Initially limit to one symbol instance,
  with explicit feedback.

## Milestone sequence and acceptance criteria

1. **Editing lifecycle:** dropdown None/Linear/Radial with future options
   disabled; immediate live preview upon entering mode; switching types
   regenerates from the same original, without accumulating copies; Commit
   and Cancel/Escape/None have the distinct effects above. Closing/switching
   dock tabs doesn't silently cancel.
2. **Stop track:** neutral visualization, existing gradient-edit gestures
   and keyboard interactions, Alpha value in a collapsible Colour Effect
   section, multi-stop interpolation. Gradient's original colour editor
   must behave exactly as it did before.
3. **Stage handles:** Linear Start/End/bar and Radial Centre/Start/Arc End;
   stage positions and panel numeric fields stay in sync.
4. **Dim Stage:** editor-only dimming with enabled-by-default checkbox.
5. Later: Grid, Path/node placement, more stop properties, easing, Chaos,
   multi-selection, optional grouping/merging.

For each milestone run relevant engine and shell tests, Clippy and UI screenshot
checks as prescribed by AGENTS.md. Add lifecycle regression tests for: an
inclusive copy count; switching modes; Escape and None restoring the original
selection/document; settings retained after cancel; Commit producing one undo
step; no accumulating copies during repeated preview edits; and correct
stop-bar behaviour in both Gradient and Array editors.

**Do not modify AGENTS.md merely to encode this feature's design.** A lasting
architectural choice can be recorded in an ADR following the repository's normal
reservation process if maintainers choose to adopt it.
